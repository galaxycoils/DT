//! Bounded 16 MB / 32-Slot LRU Texture Cache and Memoized Markdown/Table Layout Cache.
//! Ensures GPU/CPU memory stays strictly bounded and virtualized 10,000-message transcripts
//! never re-parse markdown or tables on scroll frames.

use crate::muse_render::{RenderedPart, parse_message_bubbles};
use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};
use std::collections::{HashMap, VecDeque};

/// Hard cap: 16 MiB of decoded RGBA pixel bytes across all cached textures.
pub const MAX_TEXTURE_CACHE_BYTES: usize = 16 * 1024 * 1024;
/// Hard cap: 32 texture handles maximum at any point in time.
pub const MAX_TEXTURE_CACHE_ENTRIES: usize = 32;

struct TextureEntry {
    handle: TextureHandle,
    byte_len: usize,
    width: usize,
    height: usize,
}

/// Strictly bounded LRU texture cache (`<= 16 MiB` and `<= 32` entries).
#[derive(Default)]
pub struct BoundedTextureCache {
    entries: HashMap<String, TextureEntry>,
    lru_order: VecDeque<String>,
    used_bytes: usize,
}

impl BoundedTextureCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn used_bytes(&self) -> usize {
        self.used_bytes
    }

    fn touch(&mut self, key: &str) {
        if let Some(pos) = self.lru_order.iter().position(|k| k == key) {
            self.lru_order.remove(pos);
        }
        self.lru_order.push_back(key.to_string());
    }

    /// Remove a specific texture immediately (e.g. when Live Computer or Lightbox closes).
    pub fn remove(&mut self, key: &str) -> bool {
        if let Some(removed) = self.entries.remove(key) {
            self.used_bytes = self.used_bytes.saturating_sub(removed.byte_len);
            if let Some(pos) = self.lru_order.iter().position(|k| k == key) {
                self.lru_order.remove(pos);
            }
            true
        } else {
            false
        }
    }

    /// Evict all textures whose key starts with `prefix` (e.g. `"live_computer:"`).
    pub fn evict_prefix(&mut self, prefix: &str) {
        let keys: Vec<String> = self
            .entries
            .keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect();
        for k in keys {
            self.remove(&k);
        }
    }

    /// Look up a cached texture handle and mark it as most-recently-used.
    pub fn get(&mut self, key: &str) -> Option<(egui::TextureId, [usize; 2])> {
        if self.entries.contains_key(key) {
            self.touch(key);
            let entry = self.entries.get(key)?;
            Some((entry.handle.id(), [entry.width, entry.height]))
        } else {
            None
        }
    }

    /// Insert or replace an RGBA image into the bounded LRU cache.
    pub fn insert_rgba(
        &mut self,
        ctx: &egui::Context,
        key: impl Into<String>,
        width: usize,
        height: usize,
        rgba: Vec<u8>,
    ) -> Option<(egui::TextureId, [usize; 2])> {
        let key = key.into();
        let byte_len = width.saturating_mul(height).saturating_mul(4);
        if byte_len == 0 || byte_len > MAX_TEXTURE_CACHE_BYTES || rgba.len() < byte_len {
            return None;
        }

        // Remove existing entry with the same key first so its bytes are freed before eviction.
        self.remove(&key);

        while (self.entries.len() >= MAX_TEXTURE_CACHE_ENTRIES
            || self.used_bytes.saturating_add(byte_len) > MAX_TEXTURE_CACHE_BYTES)
            && let Some(oldest) = self.lru_order.pop_front()
        {
            if let Some(evicted) = self.entries.remove(&oldest) {
                self.used_bytes = self.used_bytes.saturating_sub(evicted.byte_len);
            }
        }

        let color_image = ColorImage::from_rgba_unmultiplied([width, height], &rgba[..byte_len]);
        let handle = ctx.load_texture(key.clone(), color_image, TextureOptions::LINEAR);
        let tex_id = handle.id();
        self.used_bytes = self.used_bytes.saturating_add(byte_len);
        self.entries.insert(
            key.clone(),
            TextureEntry {
                handle,
                byte_len,
                width,
                height,
            },
        );
        self.lru_order.push_back(key);
        Some((tex_id, [width, height]))
    }

    /// Decode JPEG/PNG/WebP bytes, downscaling to `max_edge` if needed, and insert into the cache.
    pub fn insert_encoded(
        &mut self,
        ctx: &egui::Context,
        key: impl Into<String>,
        encoded_bytes: &[u8],
        max_edge: u32,
    ) -> Option<(egui::TextureId, [usize; 2])> {
        let img = image::load_from_memory(encoded_bytes).ok()?;
        let (w, h) = (img.width(), img.height());
        let resized = if w > max_edge || h > max_edge {
            img.thumbnail(max_edge, max_edge)
        } else {
            img
        };
        let rgba = resized.to_rgba8();
        let (rw, rh) = (rgba.width() as usize, rgba.height() as usize);
        self.insert_rgba(ctx, key, rw, rh, rgba.into_raw())
    }
}

/// Parsed Markdown table block for rich interactive rendering (Copy Table, CSV, Markdown, Expand ⤢).
#[derive(Debug, Clone)]
pub struct ParsedMarkdownTable {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub raw_markdown: String,
}

impl ParsedMarkdownTable {
    pub fn to_tsv(&self) -> String {
        let mut out = self.headers.join("\t");
        for row in &self.rows {
            out.push('\n');
            out.push_str(&row.join("\t"));
        }
        out
    }

    pub fn to_csv(&self) -> String {
        fn esc(cell: &str) -> String {
            if cell.contains(',') || cell.contains('"') || cell.contains('\n') {
                format!("\"{}\"", cell.replace('"', "\"\""))
            } else {
                cell.to_string()
            }
        }
        let mut out = self.headers.iter().map(|h| esc(h)).collect::<Vec<_>>().join(",");
        for row in &self.rows {
            out.push('\n');
            out.push_str(&row.iter().map(|c| esc(c)).collect::<Vec<_>>().join(","));
        }
        out
    }
}

/// Cached layout entry for a single message so virtualized `show_rows` never re-parses every frame.
#[derive(Debug, Clone)]
pub struct MemoizedMessageLayout {
    pub content_len: usize,
    pub width_bucket: u32,
    pub parts: Vec<RenderedPart>,
    pub tables: Vec<ParsedMarkdownTable>,
    pub estimated_height: f32,
}

/// Bounded memoized layout cache keyed by `message_id`.
#[derive(Default)]
pub struct MarkdownLayoutCache {
    entries: HashMap<String, MemoizedMessageLayout>,
    order: VecDeque<String>,
}

impl MarkdownLayoutCache {
    pub const MAX_ENTRIES: usize = 512;

    pub fn get_or_compute(
        &mut self,
        msg_id: &str,
        content: &str,
        has_card: bool,
        available_width: f32,
    ) -> &MemoizedMessageLayout {
        let width_bucket = ((available_width.max(200.0) / 32.0).round() as u32).max(1);
        let content_len = content.len();
        let needs_recompute = match self.entries.get(msg_id) {
            Some(existing) => {
                existing.content_len != content_len || existing.width_bucket != width_bucket
            }
            None => true,
        };

        if needs_recompute {
            let parts = parse_message_bubbles(content);
            let tables = extract_markdown_tables(content);
            let chars_per_line = ((available_width.max(240.0) / 7.5) as usize).max(24);
            let wrapped_lines: usize = content
                .lines()
                .map(|l| (l.chars().count() / chars_per_line) + 1)
                .sum::<usize>()
                .max(1);
            let mut estimated_height = 38.0 + (wrapped_lines.min(18) as f32) * 18.0;
            if !tables.is_empty() {
                estimated_height += 72.0;
            }
            if has_card {
                estimated_height += 96.0;
            }
            if self.entries.len() >= Self::MAX_ENTRIES
                && let Some(oldest) = self.order.pop_front()
            {
                self.entries.remove(&oldest);
            }
            self.order.push_back(msg_id.to_string());
            self.entries.insert(
                msg_id.to_string(),
                MemoizedMessageLayout {
                    content_len,
                    width_bucket,
                    parts,
                    tables,
                    estimated_height,
                },
            );
        }

        self.entries.get(msg_id).expect("inserted above")
    }
}

/// Extract GFM pipe tables from a markdown message.
pub fn extract_markdown_tables(text: &str) -> Vec<ParsedMarkdownTable> {
    let mut tables = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    while i + 1 < lines.len() {
        let header_line = lines[i].trim();
        let sep_line = lines[i + 1].trim();
        if header_line.starts_with('|')
            && header_line.ends_with('|')
            && sep_line.starts_with('|')
            && sep_line.contains("---")
        {
            let headers = split_pipe_row(header_line);
            let mut rows = Vec::new();
            let mut raw = format!("{header_line}\n{sep_line}");
            let mut j = i + 2;
            while j < lines.len() {
                let row_line = lines[j].trim();
                if row_line.starts_with('|') && row_line.ends_with('|') {
                    raw.push('\n');
                    raw.push_str(row_line);
                    rows.push(split_pipe_row(row_line));
                    j += 1;
                } else {
                    break;
                }
            }
            if !headers.is_empty() {
                tables.push(ParsedMarkdownTable {
                    headers,
                    rows,
                    raw_markdown: raw,
                });
            }
            i = j;
        } else {
            i += 1;
        }
    }
    tables
}

fn split_pipe_row(line: &str) -> Vec<String> {
    let trimmed = line.trim().trim_start_matches('|').trim_end_matches('|');
    trimmed
        .split('|')
        .map(|cell| cell.trim().to_string())
        .collect()
}
