//! Enforce mode: plan, summarize, rewrite and validate a request before it is
//! forwarded. Any failure here means "send the original request".

use std::sync::Arc;

use anyhow::{anyhow, bail, ensure, Result};
use bytes::Bytes;
use loom_core::{
    model::{Action, NodeKind, Provider},
    normalize::normalize,
    policy::{plan, validate_plan, PNode},
    rewrite::{rewrite_with, validate_request, Extras},
};
use loom_store::Store;
use tokio::sync::broadcast::Sender;

use crate::{
    gateway::Event,
    summarizer::{Creds, Summarizer},
};

/// What Enforce did to one request.
#[derive(Debug)]
pub struct Enforced {
    /// The rewritten body, or `None` when nothing changes (send the original bytes).
    pub body: Option<Bytes>,
    pub original_tokens: u32,
    pub sent_tokens: u32,
    pub summarized: usize,
    pub dropped: usize,
    /// Nodes whose text the user replaced.
    pub edited: usize,
    /// Tokens of the notes block appended to the request (0 when none).
    pub memory_tokens: u32,
}

/// Run one request through the full pipeline: ingest, plan, summaries, rewrite, validate.
pub async fn enforce(
    store: &Arc<Store>,
    summarizer: Option<&Arc<Summarizer>>,
    events: &Sender<Event>,
    provider: Provider,
    session: &str,
    body: &Bytes,
    creds: &Creds,
) -> Result<Enforced> {
    let t0 = std::time::Instant::now();
    let norm = normalize(provider, body).map_err(|e| anyhow!("unparseable body: {e}"))?;
    let t_norm = t0.elapsed();
    let ingested = store.ingest(session, provider, norm.model.as_deref(), &norm.msgs).await?;
    let _ = events.send(Event {
        session: session.to_string(),
        kind: "turn_completed",
        turn: ingested.turn,
        new: ingested.new,
        missing: ingested.missing,
        tokens: ingested.tokens,
    });

    let t_ingest = t0.elapsed();
    let mut data =
        store.policy_data_for_request(session, &norm.msgs).await?.ok_or_else(|| anyhow!("session has no turns"))?;
    ensure!(
        data.input.nodes.len() == norm.msgs.len(),
        "stored nodes ({}) do not match the request ({})",
        data.input.nodes.len(),
        norm.msgs.len()
    );
    ensure!(
        data.input.nodes.iter().zip(&norm.msgs).all(|(n, m)| n.kind == m.kind),
        "stored nodes do not line up with the request"
    );

    let t_load = t0.elapsed();

    // Plan; generate summaries for anything the plan wants summarized and does not have yet.
    // Summaries that are not ready in time continue in the background for the next call.
    let deadline = summarizer.map(|s| tokio::time::Instant::now() + s.cfg.timeout);
    let mut final_plan = None;
    for _ in 0..8 {
        let p = plan(&data.input);
        let missing: Vec<usize> = p
            .items
            .iter()
            .enumerate()
            .filter(|(i, it)| {
                it.decision == Action::Summarize
                    && data.input.nodes[*i].kind != NodeKind::ToolCall
                    && !data.summaries.contains_key(&it.id)
            })
            .map(|(i, _)| i)
            .collect();
        if missing.is_empty() {
            final_plan = Some(p);
            break;
        }
        match (summarizer, deadline) {
            (Some(s), Some(dl)) if tokio::time::Instant::now() < dl => {
                let jobs = missing.iter().map(|&i| {
                    let (s, node, creds) = (s.clone(), data.input.nodes[i].clone(), creds.clone());
                    // Detached: a timeout drops our wait, not the work.
                    let handle = tokio::spawn(async move { s.summarize(&node, &creds).await });
                    async move { (i, tokio::time::timeout_at(dl, handle).await) }
                });
                for (i, res) in futures_util::future::join_all(jobs).await {
                    let id = data.input.nodes[i].id.clone();
                    match res {
                        Ok(Ok(Ok((text, tokens)))) => {
                            data.input.nodes[i].summary_tokens = Some(tokens);
                            data.summaries.insert(id, text);
                        }
                        Ok(Ok(Err(e))) => {
                            tracing::debug!(%session, node = %id, error = %e, "summary unavailable");
                            data.input.no_summary.insert(id);
                        }
                        _ => {
                            tracing::debug!(%session, node = %id, "summary not ready in time");
                            data.input.no_summary.insert(id);
                        }
                    }
                }
            }
            // No summarizer, or out of time: those nodes can only be kept or dropped.
            _ => missing.iter().for_each(|&i| {
                data.input.no_summary.insert(data.input.nodes[i].id.clone());
            }),
        }
    }
    let plan = final_plan.ok_or_else(|| anyhow!("plan did not settle"))?;
    let t_plan = t0.elapsed();
    validate_plan(&data.input, &plan).map_err(|e| anyhow!("invalid plan: {e}"))?;

    let decisions: Vec<(String, Action)> = plan.items.iter().map(|i| (i.id.clone(), i.decision)).collect();
    // Tool calls in a summarized pair stay verbatim, so they do not count as summarized.
    let summarized = plan
        .items
        .iter()
        .zip(&data.input.nodes)
        .filter(|(i, n)| i.decision == Action::Summarize && n.kind != NodeKind::ToolCall)
        .count();
    let dropped = plan.items.iter().filter(|i| i.decision == Action::Drop).count();
    // The user's own edits apply to nodes that stay; a dropped node's edit goes with it.
    let edited = plan.items.iter().filter(|i| i.decision == Action::Keep && data.overrides.contains_key(&i.id)).count();
    let adds_notes = data.memory.is_some();

    if summarized == 0 && dropped == 0 && edited == 0 && !adds_notes {
        store.record_enforced(session, ingested.turn, &decisions, ingested.tokens, false).await?;
        return Ok(Enforced {
            body: None,
            original_tokens: ingested.tokens,
            sent_tokens: ingested.tokens,
            summarized,
            dropped,
            edited,
            memory_tokens: 0,
        });
    }

    let extras = Extras { overrides: Some(&data.overrides), memory: data.memory.as_deref() };
    let rewritten = rewrite_with(provider, &norm.raw, &norm.msgs, &plan, &data.summaries, &extras)
        .map_err(|e| anyhow!("rewrite failed: {e}"))?;
    validate_request(provider, &rewritten).map_err(|e| anyhow!("rewritten request is invalid: {e}"))?;
    let bytes = Bytes::from(serde_json::to_vec(&rewritten)?);
    // A rewrite that only removes or shrinks things must come out smaller; edits and notes can legitimately add text.
    if edited == 0 && !adds_notes && bytes.len() >= body.len() {
        bail!("rewritten request is not smaller ({} >= {} bytes)", bytes.len(), body.len());
    }
    let t_rewrite = t0.elapsed();
    let sent_tokens = plan.planned_tokens + data.memory_tokens;
    store.record_enforced(session, ingested.turn, &decisions, sent_tokens, true).await?;
    tracing::debug!(
        normalize_us = t_norm.as_micros() as u64,
        ingest_us = (t_ingest - t_norm).as_micros() as u64,
        load_us = (t_load - t_ingest).as_micros() as u64,
        plan_us = (t_plan - t_load).as_micros() as u64,
        rewrite_us = (t_rewrite - t_plan).as_micros() as u64,
        record_us = (t0.elapsed() - t_rewrite).as_micros() as u64,
        nodes = data.input.nodes.len(),
        "enforce timings"
    );
    Ok(Enforced {
        body: Some(bytes),
        original_tokens: ingested.tokens,
        sent_tokens,
        summarized,
        dropped,
        edited,
        memory_tokens: data.memory_tokens,
    })
}

/// Generate summaries the plan will want, ahead of time (Suggest mode: so they
/// are ready when you switch to Enforce, and for the next call).
pub async fn prefetch(store: Arc<Store>, summarizer: Arc<Summarizer>, session: String, creds: Creds) {
    let Ok(Some(data)) = store.policy_data(&session).await else { return };
    let p = plan(&data.input);
    let wanted: Vec<PNode> = p
        .items
        .iter()
        .zip(&data.input.nodes)
        .filter(|(it, n)| {
            it.decision == Action::Summarize && n.kind != NodeKind::ToolCall && !data.summaries.contains_key(&it.id)
        })
        .map(|(_, n)| n.clone())
        .collect();
    for node in wanted {
        if let Err(e) = summarizer.summarize(&node, &creds).await {
            tracing::debug!(%session, node = %node.id, error = %e, "prefetch skipped");
        }
    }
}
