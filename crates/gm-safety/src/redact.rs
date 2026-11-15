use gm_core::types::{AxElement, AxSnapshot};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;

pub fn sanitize_ax_snapshot(mut snapshot: AxSnapshot) -> AxSnapshot {
    for el in &mut snapshot.elements {
        // Rule: Never read AXSecureTextField values
        if el.is_secure || el.role == "AXSecureTextField" {
            el.value = Some("[REDACTED_SECURE_FIELD]".into());
        }
    }
    snapshot
}

pub fn filter_secure_element(el: &mut AxElement) {
    if el.is_secure || el.role == "AXSecureTextField" {
        el.value = Some("[REDACTED]".into());
    }
}

/// Thread-safe secret scrubber that replaces leased opaque vault secret values
/// with `[REDACTED:<secret_ref>]` in transcripts, tool outputs, and audit logs.
#[derive(Debug, Clone, Default)]
pub struct SecretScrubber {
    /// Maps secret_value -> secret_ref
    active_secrets: Arc<Mutex<HashMap<String, String>>>,
}

impl SecretScrubber {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a leased secret so its plaintext value is scrubbed everywhere.
    pub fn register(&self, secret_ref: impl Into<String>, secret_value: impl Into<String>) {
        let val = secret_value.into();
        if !val.trim().is_empty() {
            self.active_secrets.lock().insert(val, secret_ref.into());
        }
    }

    /// Scrub all registered secret values and `hsurr:` surrogate tokens from a string.
    pub fn scrub_str(&self, input: &str) -> String {
        let mut out = scrub_surrogates(input);
        let map = self.active_secrets.lock();
        if map.is_empty() {
            return out;
        }
        for (secret_val, secret_ref) in map.iter() {
            if !secret_val.is_empty() && out.contains(secret_val) {
                let replacement = format!("[REDACTED:{secret_ref}]");
                out = out.replace(secret_val, &replacement);
            }
        }
        out
    }

    /// Recursively scrub all string values inside a JSON payload.
    pub fn scrub_json(&self, value: &serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::String(s) => serde_json::Value::String(self.scrub_str(s)),
            serde_json::Value::Array(arr) => {
                serde_json::Value::Array(arr.iter().map(|v| self.scrub_json(v)).collect())
            }
            serde_json::Value::Object(obj) => {
                let mut scrubbed = serde_json::Map::with_capacity(obj.len());
                for (k, v) in obj {
                    scrubbed.insert(k.clone(), self.scrub_json(v));
                }
                serde_json::Value::Object(scrubbed)
            }
            other => other.clone(),
        }
    }

    /// Clear ephemeral secrets from memory when a session completes.
    pub fn clear(&self) {
        self.active_secrets.lock().clear();
    }
}

/// Scrub Muse surrogate secret handles (`hsurr:[A-Za-z0-9_-]{8,}`) to `[SURROGATE]`.
pub fn scrub_surrogates(input: &str) -> String {
    const PREFIX: &str = "hsurr:";
    if !input.contains(PREFIX) {
        return input.to_string();
    }
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < bytes.len() {
        if input[i..].starts_with(PREFIX) {
            let start_tok = i + PREFIX.len();
            let mut end_tok = start_tok;
            while end_tok < bytes.len()
                && (bytes[end_tok].is_ascii_alphanumeric()
                    || bytes[end_tok] == b'_'
                    || bytes[end_tok] == b'-')
            {
                end_tok += 1;
            }
            if end_tok - start_tok >= 8 {
                out.push_str("[SURROGATE]");
                i = end_tok;
                continue;
            }
        }
        let ch = input[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use gm_core::types::Rect;

    #[test]
    fn test_secure_field_redaction() {
        let snap = AxSnapshot {
            window_id: 1,
            app_name: "Safari".into(),
            window_title: "Login".into(),
            bounds: Rect::new(0.0, 0.0, 100.0, 100.0),
            elements: vec![AxElement {
                token: "pwd_1".into(),
                role: "AXSecureTextField".into(),
                label: Some("Password".into()),
                value: Some("super_secret_123".into()),
                frame: Rect::new(10.0, 10.0, 50.0, 20.0),
                is_enabled: true,
                is_secure: true,
            }],
            timestamp_ms: 100,
        };

        let sanitized = sanitize_ax_snapshot(snap);
        assert_eq!(
            sanitized.elements[0].value.as_deref(),
            Some("[REDACTED_SECURE_FIELD]")
        );
    }

    #[test]
    fn test_secret_scrubber_redacts_plaintext_and_json() {
        let scrubber = SecretScrubber::new();
        scrubber.register("secret://keychain/github_pat", "ghp_SuperSecretToken999");

        let raw =
            "Command output: Authenticated with ghp_SuperSecretToken999 against api.github.com";
        let scrubbed = scrubber.scrub_str(raw);
        assert!(!scrubbed.contains("ghp_SuperSecretToken999"));
        assert!(scrubbed.contains("[REDACTED:secret://keychain/github_pat]"));

        let val = serde_json::json!({
            "stdout": "token=ghp_SuperSecretToken999",
            "ok": true
        });
        let scrubbed_json = scrubber.scrub_json(&val);
        let serialized = scrubbed_json.to_string();
        assert!(!serialized.contains("ghp_SuperSecretToken999"));
        assert!(serialized.contains("[REDACTED:secret://keychain/github_pat]"));
    }

    #[test]
    fn test_scrubs_muse_hsurr_surrogates() {
        let scrubber = SecretScrubber::new();
        let line = "Authorization: Bearer hsurr:AbC123_xYz-9900 done; short=hsurr:12345";
        let scrubbed = scrubber.scrub_str(line);
        assert!(!scrubbed.contains("hsurr:AbC123_xYz-9900"));
        assert!(scrubbed.contains("Bearer [SURROGATE] done; short=hsurr:12345"));
    }
}
