//! Muse output directive rendering helpers for `gm-ui` (`T5.5`).
//! Wraps [`gm_muse::directives::parse_directives`] and validates `sandbox://`
//! attachment URIs before requesting bot file previews over daemon IPC.

pub use gm_muse::directives::RenderedPart;

/// Parse a raw assistant/channel message into bounded message bubbles (max 20 parts)
/// with extracted `sandbox://` attachments, balanced `【...】` citations, and `<options>`.
pub fn parse_message_bubbles(raw: &str) -> Vec<RenderedPart> {
    gm_muse::directives::parse_directives(raw)
}

/// Validate and convert a `sandbox://<relpath>` URI into a safe relative bot workspace path.
/// Rejects missing prefix, empty paths, NUL bytes, backslashes, and `..` traversal components.
pub fn sandbox_uri_to_bot_relpath(uri: &str) -> Option<String> {
    let trimmed = uri.trim();
    let after_scheme = trimmed.strip_prefix("sandbox://")?;
    let stripped = after_scheme.trim_start_matches('/');
    if stripped.is_empty()
        || stripped.contains('\0')
        || stripped.contains('\\')
        || stripped.contains("..")
    {
        return None;
    }
    let mut parts = Vec::new();
    for comp in std::path::Path::new(stripped).components() {
        match comp {
            std::path::Component::Normal(seg) => {
                let s = seg.to_str()?;
                if s.is_empty() || s == "." || s == ".." {
                    return None;
                }
                parts.push(s);
            }
            _ => return None,
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_or_whitespace_returns_empty() {
        assert!(parse_message_bubbles("").is_empty());
        assert!(parse_message_bubbles("   \n\t  ").is_empty());
    }

    #[test]
    fn multi_bubble_split_and_20_part_cap() {
        let joined = (1..=25)
            .map(|n| format!("bubble {n}"))
            .collect::<Vec<_>>()
            .join("<message_break/>");
        let parts = parse_message_bubbles(&joined);
        assert_eq!(parts.len(), gm_muse::directives::MAX_RENDERED_PARTS);
        assert_eq!(parts[0].text, "bubble 1");
        assert!(parts[19].text.contains("bubble 20"));
        assert!(parts[19].text.contains("bubble 25"));
    }

    #[test]
    fn sandbox_attachment_nested_brackets_and_relpath_validation() {
        let msg = "Here is ![nested [alt] label](sandbox://outbox/report(1).md) ready.";
        let parts = parse_message_bubbles(msg);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].attachments.len(), 1);
        assert_eq!(parts[0].attachments[0].alt, "nested [alt] label");
        assert_eq!(parts[0].attachments[0].uri, "sandbox://outbox/report(1).md");
        assert_eq!(
            sandbox_uri_to_bot_relpath(&parts[0].attachments[0].uri),
            Some("outbox/report(1).md".into())
        );

        for invalid in [
            "sandbox://../etc/passwd",
            "sandbox://outbox/../secret.txt",
            "sandbox:///",
            "sandbox://",
            "sandbox://a\\b",
            "sandbox://a\0b",
            "https://example.com/file.png",
        ] {
            assert_eq!(sandbox_uri_to_bot_relpath(invalid), None, "for {invalid}");
        }
    }

    #[test]
    fn balanced_and_unclosed_citations() {
        let msg = "Fact one【calendar:today】and unclosed 【dangling quote.";
        let parts = parse_message_bubbles(msg);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].citations.len(), 1);
        assert_eq!(parts[0].citations[0].index, 1);
        assert_eq!(parts[0].citations[0].raw, "calendar:today");
        assert!(parts[0].text.contains("Fact one[1]"));
        assert!(parts[0].text.contains("【dangling quote."));
    }

    #[test]
    fn options_and_literal_html_tags() {
        let msg = "Check <img src=x onerror=alert(1)> safely.\n<options><option>Yes</option><option>No</option></options>";
        let parts = parse_message_bubbles(msg);
        assert_eq!(parts.len(), 1);
        assert!(parts[0].text.contains("<img src=x onerror=alert(1)>"));
        assert_eq!(parts[0].options, vec!["Yes".to_string(), "No".to_string()]);
    }
}
