//! Content identity and matching of request messages to stored nodes.

use std::collections::HashMap;

use crate::model::{CanonMsg, Provider};

/// blake3 over role, kind, source, tool id and content (length-prefixed fields).
pub fn canonical_hash(m: &CanonMsg) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    for part in [
        m.role.as_str(),
        m.kind.as_str(),
        m.source.as_deref().unwrap_or(""),
        m.tool_id.as_deref().unwrap_or(""),
        m.content.as_str(),
    ] {
        h.update(&(part.len() as u64).to_le_bytes());
        h.update(part.as_bytes());
    }
    *h.finalize().as_bytes()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Match {
    Existing(String),
    New,
}

/// Agents may repeat identical messages ("ok"). The Nth copy of a hash within
/// a request is matched to the Nth stored node with that hash.
pub fn occurrences(hashes: &[[u8; 32]]) -> Vec<u32> {
    let mut seen: HashMap<[u8; 32], u32> = HashMap::new();
    hashes
        .iter()
        .map(|h| {
            let c = seen.entry(*h).or_insert(0);
            *c += 1;
            *c - 1
        })
        .collect()
}

pub fn match_nodes(
    hashes: &[[u8; 32]],
    occs: &[u32],
    existing: &HashMap<([u8; 32], u32), String>,
) -> Vec<Match> {
    hashes
        .iter()
        .zip(occs)
        .map(|(h, o)| match existing.get(&(*h, *o)) {
            Some(id) => Match::Existing(id.clone()),
            None => Match::New,
        })
        .collect()
}

/// Rough token estimate (len/4). Provider tokenizers come later.
pub fn count_tokens(text: &str) -> u32 {
    text.len().div_ceil(4) as u32
}

/// Per-message framing tokens.
pub fn message_overhead(_provider: Provider) -> u32 {
    4
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{NodeKind, Role};

    fn m(content: &str) -> CanonMsg {
        CanonMsg { role: Role::User, kind: NodeKind::User, content: content.into(), source: None, tool_id: None, loc: crate::model::Loc::System, opaque: false }
    }

    #[test]
    fn hash_is_stable_and_field_sensitive() {
        assert_eq!(canonical_hash(&m("a")), canonical_hash(&m("a")));
        assert_ne!(canonical_hash(&m("a")), canonical_hash(&m("b")));
        let mut x = m("a");
        x.source = Some("s".into());
        assert_ne!(canonical_hash(&m("a")), canonical_hash(&x));
    }

    #[test]
    fn repeated_messages_get_distinct_occurrences() {
        let (a, b) = (canonical_hash(&m("ok")), canonical_hash(&m("x")));
        assert_eq!(occurrences(&[a, b, a, a]), [0, 0, 1, 2]);
    }

    #[test]
    fn matches_existing_by_hash_and_occurrence() {
        let a = canonical_hash(&m("ok"));
        let existing = HashMap::from([((a, 0), "n0".to_string())]);
        let r = match_nodes(&[a, a], &[0, 1], &existing);
        assert_eq!(r, [Match::Existing("n0".into()), Match::New]);
    }
}
