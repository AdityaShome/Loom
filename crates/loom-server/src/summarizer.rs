//! Summaries of old context, produced by a cheap model and cached by content hash.
//!
//! Configure with environment variables (no summarizer is used without a model):
//!   LOOM_SUMMARIZER_MODEL        model name (enables summarizing)
//!   LOOM_SUMMARIZER_PROVIDER     `openai` (default; also Ollama/vLLM) or `anthropic`
//!   LOOM_SUMMARIZER_URL          base URL (default: the provider's upstream)
//!   LOOM_SUMMARIZER_API_KEY      optional; otherwise the triggering request's own
//!                                credentials are reused when the providers match
//!   LOOM_SUMMARIZER_TIMEOUT_MS   how long a request may wait for summaries (default 300)
//!   LOOM_SUMMARIZER_EXTRA        JSON object merged into the request body, e.g.
//!                                {"reasoning_effort":"none"} (default for Qwen3 models, so
//!                                thinking does not use up the reply)

use std::{collections::HashSet, sync::Arc, time::Duration};

use anyhow::{anyhow, bail, Result};
use axum::http::{HeaderMap, HeaderValue};
use loom_core::{
    matching::{count_tokens, message_overhead},
    model::{NodeKind, Provider},
    policy::PNode,
    rewrite::render_summary,
};
use loom_store::Store;
use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub struct SummarizerConfig {
    pub provider: Provider,
    pub url: String,
    pub model: String,
    pub api_key: Option<String>,
    /// How long a request waits for summaries before falling back to keep/drop.
    pub timeout: Duration,
    /// Extra fields merged into the summarizer request body.
    pub extra: serde_json::Map<String, Value>,
}

impl SummarizerConfig {
    pub fn from_env(anthropic_upstream: &str, openai_upstream: &str) -> Option<Self> {
        let model = std::env::var("LOOM_SUMMARIZER_MODEL").ok().filter(|m| !m.trim().is_empty())?;
        let provider = match std::env::var("LOOM_SUMMARIZER_PROVIDER").unwrap_or_default().to_lowercase().as_str() {
            "anthropic" => Provider::Anthropic,
            _ => Provider::OpenAi,
        };
        let url = std::env::var("LOOM_SUMMARIZER_URL").unwrap_or_else(|_| match provider {
            Provider::Anthropic => anthropic_upstream.to_string(),
            Provider::OpenAi => openai_upstream.to_string(),
        });
        let timeout = std::env::var("LOOM_SUMMARIZER_TIMEOUT_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(300);
        // Thinking models spend the whole reply budget reasoning unless told not to.
        let mut extra = serde_json::Map::new();
        if provider == Provider::OpenAi && model.to_lowercase().contains("qwen3") {
            extra.insert("reasoning_effort".into(), Value::String("none".into()));
        }
        if let Some(Value::Object(o)) =
            std::env::var("LOOM_SUMMARIZER_EXTRA").ok().and_then(|v| serde_json::from_str(&v).ok())
        {
            extra.extend(o);
        }
        Some(Self {
            provider,
            url: url.trim_end_matches('/').to_string(),
            model,
            api_key: std::env::var("LOOM_SUMMARIZER_API_KEY").ok().filter(|k| !k.is_empty()),
            timeout: Duration::from_millis(timeout),
            extra,
        })
    }
}

/// Credentials of the request that triggered summarizing. Kept in memory only,
/// and only used when the summarizer talks to the same provider.
#[derive(Debug, Clone, Default)]
pub struct Creds {
    pub provider: Option<Provider>,
    pub authorization: Option<HeaderValue>,
    pub x_api_key: Option<HeaderValue>,
    pub anthropic_version: Option<HeaderValue>,
}

impl Creds {
    pub fn from_headers(provider: Provider, h: &HeaderMap) -> Self {
        Self {
            provider: Some(provider),
            authorization: h.get("authorization").cloned(),
            x_api_key: h.get("x-api-key").cloned(),
            anthropic_version: h.get("anthropic-version").cloned(),
        }
    }
}

pub struct Summarizer {
    pub cfg: SummarizerConfig,
    client: reqwest::Client,
    store: Arc<Store>,
    inflight: std::sync::Mutex<HashSet<[u8; 32]>>,
}

fn kind_label(k: NodeKind) -> &'static str {
    match k {
        NodeKind::File => "file contents",
        NodeKind::ToolResult => "tool output",
        NodeKind::Memory => "memory entry",
        NodeKind::Assistant => "assistant message",
        NodeKind::User => "user message",
        _ => "text",
    }
}

/// Keep the start and end of very long inputs.
fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max * 2 / 3).collect();
    let tail: String = text.chars().rev().take(max / 3).collect::<Vec<_>>().into_iter().rev().collect();
    format!("{head}\n...[middle omitted]...\n{tail}")
}

/// Remove reasoning blocks some local models put in the reply.
fn strip_think(s: &str) -> String {
    let mut out = s.to_string();
    while let (Some(a), Some(b)) = (out.find("<think>"), out.find("</think>")) {
        if b < a {
            break;
        }
        out.replace_range(a..b + "</think>".len(), "");
    }
    // An unterminated block means the model ran out of room while thinking.
    if let Some(a) = out.find("<think>") {
        out.truncate(a);
    }
    out.trim().to_string()
}

impl Summarizer {
    pub fn new(cfg: SummarizerConfig, store: Arc<Store>) -> Self {
        let client = reqwest::Client::builder().timeout(Duration::from_secs(120)).build().expect("http client");
        Self { cfg, client, store, inflight: Default::default() }
    }

    /// Summarize one node and cache the result. Fails if another call for the
    /// same content is already running, or the summary is not smaller.
    pub async fn summarize(&self, node: &PNode, creds: &Creds) -> Result<(String, u32)> {
        if let Some((text, tokens)) = self.store.get_summary(&node.content_hash).await? {
            return Ok((text, tokens));
        }
        if !self.inflight.lock().unwrap().insert(node.content_hash) {
            bail!("summary already being generated");
        }
        let res = self.generate(node, creds).await;
        self.inflight.lock().unwrap().remove(&node.content_hash);
        let text = res.inspect_err(|e| tracing::warn!(node = %node.id, error = %e, "summary failed"))?;
        let tokens = count_tokens(&render_summary(&text)) + message_overhead(self.cfg.provider);
        if tokens >= node.tokens {
            tracing::warn!(node = %node.id, summary_tokens = tokens, original = node.tokens, "summary not smaller; discarded");
            bail!("summary ({tokens} tokens) is not smaller than the original ({})", node.tokens);
        }
        tracing::info!(node = %node.id, from = node.tokens, to = tokens, model = %self.cfg.model, "summary cached");
        self.store.put_summary(&node.content_hash, &text, tokens, &self.cfg.model).await?;
        Ok((text, tokens))
    }

    async fn generate(&self, node: &PNode, creds: &Creds) -> Result<String> {
        let words = ((node.tokens as f32 / 5.0) * 0.75).clamp(25.0, 250.0) as usize;
        let system = format!(
            "You compress context for an AI agent's working memory. Summarize the following {} faithfully. \
             Keep file paths, identifiers, function names, numbers, commands and error messages exactly as written. \
             Drop boilerplate and repetition. Reply with the summary only, at most {words} words.",
            kind_label(node.kind)
        );
        let user = clip(&node.content, 16_000);
        let max_tokens = (words as f32 * 1.7) as u32 + 60;

        let (url, mut body, mut headers) = match self.cfg.provider {
            Provider::OpenAi => (
                format!("{}/v1/chat/completions", self.cfg.url),
                json!({
                    "model": self.cfg.model, "stream": false, "temperature": 0, "max_tokens": max_tokens,
                    "messages": [{"role": "system", "content": system}, {"role": "user", "content": user}],
                }),
                HeaderMap::new(),
            ),
            Provider::Anthropic => (
                format!("{}/v1/messages", self.cfg.url),
                json!({
                    "model": self.cfg.model, "max_tokens": max_tokens, "temperature": 0, "system": system,
                    "messages": [{"role": "user", "content": user}],
                }),
                HeaderMap::new(),
            ),
        };
        if let Some(o) = body.as_object_mut() {
            o.extend(self.cfg.extra.clone());
        }
        self.auth(&mut headers, creds);

        let resp = self.client.post(&url).headers(headers).json(&body).send().await?;
        let status = resp.status();
        let v: Value = resp.json().await.map_err(|e| anyhow!("summarizer returned non-JSON ({status}): {e}"))?;
        if !status.is_success() {
            bail!("summarizer returned {status}: {}", v.to_string().chars().take(200).collect::<String>());
        }
        let text = match self.cfg.provider {
            Provider::OpenAi => v.pointer("/choices/0/message/content").and_then(Value::as_str),
            Provider::Anthropic => v.pointer("/content/0/text").and_then(Value::as_str),
        }
        .ok_or_else(|| anyhow!("summarizer reply has no text"))?;
        let text = strip_think(text);
        if text.is_empty() {
            bail!("summarizer returned an empty summary");
        }
        Ok(text)
    }

    fn auth(&self, h: &mut HeaderMap, creds: &Creds) {
        let bearer = |k: &str| HeaderValue::from_str(&format!("Bearer {k}")).ok();
        match (&self.cfg.api_key, self.cfg.provider) {
            (Some(k), Provider::OpenAi) => {
                if let Some(v) = bearer(k) {
                    h.insert("authorization", v);
                }
            }
            (Some(k), Provider::Anthropic) => {
                if let Ok(v) = HeaderValue::from_str(k) {
                    h.insert("x-api-key", v);
                }
                h.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
            }
            // No dedicated key: reuse the caller's credentials, but only toward the same provider.
            (None, p) if creds.provider == Some(p) => {
                for (name, v) in [
                    ("authorization", &creds.authorization),
                    ("x-api-key", &creds.x_api_key),
                    ("anthropic-version", &creds.anthropic_version),
                ] {
                    if let Some(v) = v {
                        h.insert(name, v.clone());
                    }
                }
            }
            _ => {}
        }
        if self.cfg.provider == Provider::Anthropic && !h.contains_key("anthropic-version") {
            h.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn think_blocks_are_stripped() {
        assert_eq!(strip_think("<think>hmm</think>\nThe answer."), "The answer.");
        assert_eq!(strip_think("plain"), "plain");
        assert_eq!(strip_think("<think>never closed"), "");
        assert_eq!(strip_think("a<think>x</think>b<think>y</think>c"), "abc");
    }

    #[test]
    fn long_text_is_clipped_in_the_middle() {
        let t = "a".repeat(100) + &"b".repeat(100);
        let c = clip(&t, 30);
        assert!(c.starts_with("aaaa") && c.ends_with("bbbb") && c.contains("middle omitted"));
        assert_eq!(clip("short", 30), "short");
    }
}
