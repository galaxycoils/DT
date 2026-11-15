//! Rich Muse output directive scanner and renderer (`<message_break/>`,
//! `sandbox://` image/file attachments, `【…】` citations, and `<options>`).
//! Uses a hand-written O(n) scanner; unknown tags stay literal; capped at 20 parts.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Maximum number of message bubbles emitted from a single response.
/// Any excess parts beyond 20 are merged into the 20th part.
pub const MAX_RENDERED_PARTS: usize = 20;

const MESSAGE_BREAK_TAG: &str = "<message_break/>";
const MESSAGE_BREAK_SPACED_TAG: &str = "<message_break />";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "surface", rename_all = "snake_case")]
pub enum RenderSurface {
    NativeUi,
    CompanionWeb,
    Terminal,
    Bridge { platform: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentRef {
    pub alt: String,
    pub uri: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CitationRef {
    pub index: usize,
    pub raw: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RenderedPart {
    pub text: String,
    #[serde(default)]
    pub attachments: Vec<AttachmentRef>,
    #[serde(default)]
    pub citations: Vec<CitationRef>,
    #[serde(default)]
    pub options: Vec<String>,
}

/// Split raw text on `<message_break/>` (hand-written scan) and cap at [`MAX_RENDERED_PARTS`],
/// merging any remainder beyond 20 into the final segment.
fn split_capped_segments(text: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut rest_start = 0usize;
    let bytes = text.as_bytes();
    let mut i = 0usize;

    while i < bytes.len() {
        if bytes[i] == b'<' {
            let tail = &text[i..];
            let matched_len = if tail.starts_with(MESSAGE_BREAK_TAG) {
                Some(MESSAGE_BREAK_TAG.len())
            } else if tail.starts_with(MESSAGE_BREAK_SPACED_TAG) {
                Some(MESSAGE_BREAK_SPACED_TAG.len())
            } else {
                None
            };
            if let Some(len) = matched_len {
                let seg = text[rest_start..i].trim();
                if !seg.is_empty() {
                    segments.push(seg.to_string());
                }
                i += len;
                rest_start = i;
                continue;
            }
        }
        // Advance cleanly to next UTF-8 character boundary
        let ch_len = text[i..].chars().next().map_or(1, char::len_utf8);
        i += ch_len;
    }

    let last_seg = text[rest_start..].trim();
    if !last_seg.is_empty() {
        segments.push(last_seg.to_string());
    }

    if segments.len() > MAX_RENDERED_PARTS {
        let tail = segments.split_off(MAX_RENDERED_PARTS - 1);
        segments.push(tail.join("\n\n"));
    }

    segments
}

/// Attempt to parse `![alt](sandbox://...)` starting at `start` (pointing at `!`).
/// Enforces balanced brackets inside `alt` (single line) and balanced parentheses in the URI.
/// Returns `Some((AttachmentRef, next_byte_idx))` on match.
fn try_scan_sandbox_attachment(s: &str, start: usize) -> Option<(AttachmentRef, usize)> {
    let tail = s.get(start..)?;
    if !tail.starts_with("![") {
        return None;
    }
    let alt_start = start + 2;
    let mut depth = 1usize;
    let mut close_bracket = None;

    for (rel_idx, ch) in s[alt_start..].char_indices() {
        if ch == '\n' || ch == '\r' {
            // Attachment alt text stays on a single line so unclosed `[` cannot cause quadratic scans.
            return None;
        }
        match ch {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    close_bracket = Some(alt_start + rel_idx);
                    break;
                }
            }
            _ => {}
        }
    }

    let close_bracket = close_bracket?;
    let after_bracket = close_bracket + 1;
    let paren_tail = s.get(after_bracket..)?;
    if !paren_tail.starts_with("(sandbox://") {
        return None;
    }

    let uri_start = after_bracket + 1;
    let mut p_depth = 1usize;
    let mut close_paren = None;
    for (rel_idx, ch) in s[uri_start..].char_indices() {
        if ch == '\n' || ch == '\r' || ch == '\0' {
            return None;
        }
        match ch {
            '(' => p_depth += 1,
            ')' => {
                p_depth -= 1;
                if p_depth == 0 {
                    close_paren = Some(uri_start + rel_idx);
                    break;
                }
            }
            _ => {}
        }
    }

    let close_paren = close_paren?;
    let uri = s[uri_start..close_paren].trim().to_string();
    if uri == "sandbox://" || uri.chars().any(char::is_whitespace) {
        return None;
    }
    let alt = s[alt_start..close_bracket].trim().to_string();
    Some((AttachmentRef { alt, uri }, close_paren + 1))
}

/// Attempt to parse balanced `【...】` citation starting at `start` (pointing at `【`).
/// Returns `None` if unclosed, leaving `【` and following text literal.
fn try_scan_citation(s: &str, start: usize) -> Option<(String, usize)> {
    let tail = s.get(start..)?;
    let open_len = if tail.starts_with('【') {
        '【'.len_utf8()
    } else {
        return None;
    };
    let inner_start = start + open_len;
    let mut depth = 1usize;
    for (rel_idx, ch) in s[inner_start..].char_indices() {
        match ch {
            '【' => depth += 1,
            '】' => {
                depth -= 1;
                if depth == 0 {
                    let inner_end = inner_start + rel_idx;
                    let raw = s[inner_start..inner_end].trim();
                    if raw.is_empty() {
                        return None;
                    }
                    return Some((raw.to_string(), inner_end + '】'.len_utf8()));
                }
            }
            _ => {}
        }
    }
    None
}

/// Attempt to scan `<options>...</options>` starting at `start`.
/// Supports `<option>...</option>` child tags or line-delimited choices inside `<options>...</options>`.
/// Returns `None` if no matching `</options>` tag exists so incomplete markup stays literal.
fn try_scan_options_block(s: &str, start: usize) -> Option<(Vec<String>, usize)> {
    let open_tag = "<options>";
    let close_tag = "</options>";
    let tail = s.get(start..)?;
    if !tail.starts_with(open_tag) {
        return None;
    }
    let inner_start = start + open_tag.len();
    let close_rel = s[inner_start..].find(close_tag)?;
    let inner = &s[inner_start..(inner_start + close_rel)];
    let end_idx = inner_start + close_rel + close_tag.len();

    let mut extracted = Vec::new();
    let mut pos = 0usize;
    while let Some(opt_rel) = inner[pos..].find("<option>") {
        let item_start = pos + opt_rel + "<option>".len();
        if let Some(opt_end_rel) = inner[item_start..].find("</option>") {
            let label = inner[item_start..(item_start + opt_end_rel)].trim();
            if !label.is_empty() {
                extracted.push(label.to_string());
            }
            pos = item_start + opt_end_rel + "</option>".len();
        } else {
            break;
        }
    }

    if extracted.is_empty() {
        for raw_line in inner.lines() {
            let trimmed = raw_line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let cleaned = trimmed
                .strip_prefix("- ")
                .or_else(|| trimmed.strip_prefix("* "))
                .unwrap_or_else(|| {
                    if let Some((digits, rest)) = trimmed.split_once(". ")
                        && !digits.is_empty()
                        && digits.chars().all(|c| c.is_ascii_digit())
                    {
                        rest
                    } else {
                        trimmed
                    }
                })
                .trim();
            if !cleaned.is_empty() {
                extracted.push(cleaned.to_string());
            }
        }
    }

    Some((extracted, end_idx))
}

/// Normalize whitespace around stripped directive lines without altering literal text lines.
fn clean_part_text(raw: &str) -> String {
    let mut lines: Vec<&str> = Vec::new();
    let mut prev_blank = true;
    for line in raw.lines() {
        let trimmed_end = line.trim_end();
        if trimmed_end.trim().is_empty() {
            if !prev_blank {
                lines.push("");
                prev_blank = true;
            }
        } else {
            lines.push(trimmed_end);
            prev_blank = false;
        }
    }
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines.join("\n").trim().to_string()
}

fn scan_segment(segment: &str) -> RenderedPart {
    let mut text_buf = String::with_capacity(segment.len());
    let mut attachments = Vec::new();
    let mut citations = Vec::new();
    let mut citation_index_map: BTreeMap<String, usize> = BTreeMap::new();
    let mut options = Vec::new();

    let mut i = 0usize;
    while i < segment.len() {
        let ch = segment[i..].chars().next().unwrap();

        if ch == '!'
            && let Some((att, next_idx)) = try_scan_sandbox_attachment(segment, i)
        {
            attachments.push(att);
            i = next_idx;
            continue;
        }

        if ch == '【'
            && let Some((raw_cite, next_idx)) = try_scan_citation(segment, i)
        {
            let index = if let Some(&existing) = citation_index_map.get(&raw_cite) {
                existing
            } else {
                let next_num = citations.len() + 1;
                citation_index_map.insert(raw_cite.clone(), next_num);
                citations.push(CitationRef {
                    index: next_num,
                    raw: raw_cite,
                });
                next_num
            };
            text_buf.push_str(&format!("[{index}]"));
            i = next_idx;
            continue;
        }

        if ch == '<'
            && let Some((opts, next_idx)) = try_scan_options_block(segment, i)
        {
            options.extend(opts);
            i = next_idx;
            continue;
        }

        // Everything else (including unknown `<foo>` tags, unclosed `【`, literal HTML) stays verbatim.
        text_buf.push(ch);
        i += ch.len_utf8();
    }

    RenderedPart {
        text: clean_part_text(&text_buf),
        attachments,
        citations,
        options,
    }
}

/// Hand-written O(n) parser for Muse response directives:
/// - splits on `<message_break/>` (capped at 20 parts, merging any remainder into part 20)
/// - extracts `![alt](sandbox://...)` attachments (supporting nested `[alt]` brackets)
/// - extracts balanced `【...】` citations (unclosed `【` stays literal)
/// - extracts `<options>...</options>` choices
/// - leaves unknown HTML/XML tags as literal text.
pub fn parse_directives(text: &str) -> Vec<RenderedPart> {
    if text.trim().is_empty() {
        return Vec::new();
    }
    split_capped_segments(text)
        .into_iter()
        .map(|seg| scan_segment(&seg))
        .filter(|p| {
            !p.text.is_empty()
                || !p.attachments.is_empty()
                || !p.citations.is_empty()
                || !p.options.is_empty()
        })
        .collect()
}

/// Render parsed directive parts into plain text strings for `Terminal` and `Bridge` surfaces
/// (and fallback text views):
/// - citations appear as `[n]` inline with a `Sources:` footer
/// - attachments appear as `(file: <uri>)`
/// - options appear as a numbered list
pub fn render_plain(parts: &[RenderedPart], _surface: &RenderSurface) -> Vec<String> {
    parts
        .iter()
        .filter_map(|part| {
            let mut blocks: Vec<String> = Vec::new();

            if !part.text.is_empty() {
                blocks.push(part.text.clone());
            }

            if !part.attachments.is_empty() {
                let att_lines: Vec<String> = part
                    .attachments
                    .iter()
                    .map(|att| {
                        if att.alt.is_empty() {
                            format!("(file: {})", att.uri)
                        } else {
                            format!("{} (file: {})", att.alt, att.uri)
                        }
                    })
                    .collect();
                blocks.push(att_lines.join("\n"));
            }

            if !part.options.is_empty() {
                let opt_lines: Vec<String> = part
                    .options
                    .iter()
                    .enumerate()
                    .map(|(idx, opt)| format!("{}. {}", idx + 1, opt))
                    .collect();
                blocks.push(opt_lines.join("\n"));
            }

            if !part.citations.is_empty() {
                let cite_lines: Vec<String> = part
                    .citations
                    .iter()
                    .map(|c| format!("[{}] {}", c.index, c.raw))
                    .collect();
                blocks.push(format!("Sources:\n{}", cite_lines.join("\n")));
            }

            if blocks.is_empty() {
                None
            } else {
                Some(blocks.join("\n\n"))
            }
        })
        .collect()
}

/// Convenience helper parsing raw Muse text and rendering for a plain surface in one call.
pub fn render_text_for_surface(text: &str, surface: &RenderSurface) -> Vec<String> {
    let parts = parse_directives(text);
    render_plain(&parts, surface)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_string_produces_empty_output() {
        assert!(parse_directives("").is_empty());
        assert!(parse_directives("   \n\t  ").is_empty());
        assert!(render_text_for_surface("", &RenderSurface::Terminal).is_empty());
    }

    #[test]
    fn handles_nested_brackets_unclosed_citation_and_unknown_tags_literally() {
        let raw = "Hello <custom_tag>world</custom_tag> with <img src=x onerror=alert(1)>!\n\
                   Here is nested attachment ![Chart [Q4 [final]]](sandbox://outbox/q4.png) and cite.【doc1†L10】\n\
                   Plus unclosed citation 【incomplete and stays literal";
        let parts = parse_directives(raw);
        assert_eq!(parts.len(), 1);
        let p = &parts[0];
        assert_eq!(p.attachments.len(), 1);
        assert_eq!(p.attachments[0].alt, "Chart [Q4 [final]]");
        assert_eq!(p.attachments[0].uri, "sandbox://outbox/q4.png");
        assert_eq!(p.citations.len(), 1);
        assert_eq!(p.citations[0].raw, "doc1†L10");
        assert!(p.text.contains("<custom_tag>world</custom_tag>"));
        assert!(p.text.contains("<img src=x onerror=alert(1)>"));
        assert!(p.text.contains("[1]"));
        assert!(p.text.contains("【incomplete and stays literal"));

        let rendered = render_plain(
            &parts,
            &RenderSurface::Bridge {
                platform: "slack".into(),
            },
        );
        assert_eq!(rendered.len(), 1);
        assert!(rendered[0].contains("(file: sandbox://outbox/q4.png)"));
        assert!(rendered[0].contains("Sources:\n[1] doc1†L10"));
        assert!(rendered[0].contains("【incomplete and stays literal"));
    }

    #[test]
    fn renders_options_citations_dedup_and_splits_bubbles() {
        let raw = "First bubble.【src-a†L1】 Also duplicate cite.【src-a†L1】 Next.【src-b†L5】\
                   <message_break/>\
                   Second bubble.<options><option>Approve</option><option>Edit</option></options>";
        let parts = parse_directives(raw);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].citations.len(), 2);
        assert!(parts[0].text.contains("[1]"));
        assert!(parts[0].text.contains("[2]"));
        assert_eq!(parts[1].options, vec!["Approve", "Edit"]);

        let plain = render_plain(&parts, &RenderSurface::Terminal);
        assert_eq!(plain.len(), 2);
        assert!(plain[0].contains("Sources:\n[1] src-a†L1\n[2] src-b†L5"));
        assert!(plain[1].contains("1. Approve\n2. Edit"));
    }

    #[test]
    fn ten_thousand_breaks_caps_at_20_parts_and_merges_tail() {
        let mut raw = String::new();
        for idx in 0..10_000 {
            if idx > 0 {
                raw.push_str("<message_break/>");
            }
            raw.push_str(&format!("bubble-{idx}"));
        }
        let parts = parse_directives(&raw);
        assert_eq!(parts.len(), MAX_RENDERED_PARTS);
        assert_eq!(parts[0].text, "bubble-0");
        assert_eq!(parts[18].text, "bubble-18");
        assert!(parts[19].text.contains("bubble-19"));
        assert!(parts[19].text.contains("bubble-9999"));
    }
}
