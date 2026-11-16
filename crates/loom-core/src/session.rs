use serde_json::Value;

/// Header an agent or SDK can set to name its session explicitly.
pub const SESSION_HEADER: &str = "x-loom-session";

/// Resolve a session id. An explicit header wins; otherwise hash the system
/// prompt plus the first user message. Returns `None` if the body gives
/// nothing to hash (not JSON, or no messages).
pub fn resolve_session(header: Option<&str>, body: &[u8]) -> Option<String> {
    if let Some(h) = header.map(str::trim).filter(|h| !h.is_empty()) {
        return Some(h.to_string());
    }
    let v: Value = serde_json::from_slice(body).ok()?;
    let system = v.get("system").map(canonical).unwrap_or_default();
    let first_user = v
        .get("messages")?
        .as_array()?
        .iter()
        .find(|m| m.get("role").and_then(Value::as_str) == Some("user"))
        .and_then(|m| m.get("content"))
        .map(canonical)?;
    let mut h = blake3::Hasher::new();
    h.update(system.as_bytes());
    h.update(&[0]);
    h.update(first_user.as_bytes());
    Some(format!("auto-{}", &h.finalize().to_hex()[..16]))
}

/// Stable string form of a JSON value (serde_json sorts keys without
/// `preserve_order`, so equal values serialize identically).
fn canonical(v: &Value) -> String {
    match v {
        Value::String(s) => s.trim().to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_wins() {
        assert_eq!(resolve_session(Some("abc"), b"junk").as_deref(), Some("abc"));
    }

    #[test]
    fn stable_across_turns() {
        let a = br#"{"system":"S","messages":[{"role":"user","content":"hi"}]}"#;
        let b = br#"{"system":"S","messages":[{"role":"user","content":"hi"},{"role":"assistant","content":"yo"},{"role":"user","content":"more"}]}"#;
        assert_eq!(resolve_session(None, a), resolve_session(None, b));
        assert!(resolve_session(None, a).is_some());
    }

    #[test]
    fn differs_by_system() {
        let a = br#"{"system":"S1","messages":[{"role":"user","content":"hi"}]}"#;
        let b = br#"{"system":"S2","messages":[{"role":"user","content":"hi"}]}"#;
        assert_ne!(resolve_session(None, a), resolve_session(None, b));
    }

    #[test]
    fn garbage_is_none() {
        assert_eq!(resolve_session(None, b"not json"), None);
        assert_eq!(resolve_session(None, br#"{"messages":[]}"#), None);
    }
}
