//! User-initiated native file imports. Exposed only over local authenticated IPC.
use crate::OpenBotSupervisor;
use gm_core::error::{GmError, Result};
use std::{io::Read, path::Path};
impl OpenBotSupervisor {
    pub fn import_attachment(&self, bot_id: &str, source: &Path) -> Result<String> {
        let sandbox = self.roster().workspace_sandbox(bot_id)?;
        if std::fs::symlink_metadata(source)?.file_type().is_symlink() {
            return Err(GmError::SafetyViolation(
                "Attachment imports reject symbolic links".into(),
            ));
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = options.open(source)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() > 8 * 1024 * 1024 {
            return Err(GmError::Config(
                "Attachments must be regular files under 8 MiB".into(),
            ));
        }
        let mut bytes = vec![];
        file.take(8 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > 8 * 1024 * 1024 {
            return Err(GmError::Config("Attachment exceeds 8 MiB".into()));
        }
        let original = source
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file");
        let clean: String = original
            .chars()
            .take(80)
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let name = format!("attachment-{}-{clean}", uuid::Uuid::new_v4());
        sandbox.receive_artifact(std::ffi::OsStr::new(&name), &bytes)?;
        Ok(format!("inbox/{name}"))
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentPreviewKind {
    Text,
    Image,
    Pdf,
    Binary,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct BoundedAttachmentPreview {
    pub kind: AttachmentPreviewKind,
    pub mime_type: String,
    pub byte_size: usize,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub pdf_pages_estimate: Option<usize>,
    pub text_snippet: Option<String>,
    pub truncated: bool,
}

pub const MAX_PREVIEW_INPUT_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_PREVIEW_TEXT_CHARS: usize = 4096;

/// Produce a bounded passive preview for text, image, or PDF attachments without executing embedded code.
pub fn preview_attachment_bounded(
    filename: &str,
    bytes: &[u8],
) -> Result<BoundedAttachmentPreview> {
    if bytes.len() > MAX_PREVIEW_INPUT_BYTES {
        return Err(GmError::SafetyViolation(
            "Attachment preview exceeds 8 MiB passive cap".into(),
        ));
    }
    let lower = filename.to_ascii_lowercase();
    if bytes.starts_with(b"%PDF-") || lower.ends_with(".pdf") {
        if !bytes.starts_with(b"%PDF-") {
            return Err(GmError::Config("Invalid PDF header".into()));
        }
        let lossy = String::from_utf8_lossy(bytes);
        let pages = lossy
            .matches("/Type /Page")
            .count()
            .saturating_sub(lossy.matches("/Type /Pages").count())
            .max(1);
        let printable: String = lossy
            .chars()
            .filter(|c| c.is_ascii_graphic() || *c == ' ' || *c == '\n')
            .take(MAX_PREVIEW_TEXT_CHARS)
            .collect();
        return Ok(BoundedAttachmentPreview {
            kind: AttachmentPreviewKind::Pdf,
            mime_type: "application/pdf".into(),
            byte_size: bytes.len(),
            width: None,
            height: None,
            pdf_pages_estimate: Some(pages),
            text_snippet: Some(printable),
            truncated: lossy.len() > MAX_PREVIEW_TEXT_CHARS,
        });
    }
    if let Ok(format) = image::guess_format(bytes)
        && let Ok((w, h)) =
            image::ImageReader::with_format(std::io::Cursor::new(bytes), format).into_dimensions()
    {
        return Ok(BoundedAttachmentPreview {
            kind: AttachmentPreviewKind::Image,
            mime_type: format.to_mime_type().to_string(),
            byte_size: bytes.len(),
            width: Some(w),
            height: Some(h),
            pdf_pages_estimate: None,
            text_snippet: None,
            truncated: false,
        });
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        let snippet: String = text.chars().take(MAX_PREVIEW_TEXT_CHARS).collect();
        return Ok(BoundedAttachmentPreview {
            kind: AttachmentPreviewKind::Text,
            mime_type: "text/plain; charset=utf-8".into(),
            byte_size: bytes.len(),
            width: None,
            height: None,
            pdf_pages_estimate: None,
            text_snippet: Some(snippet),
            truncated: text.chars().count() > MAX_PREVIEW_TEXT_CHARS,
        });
    }
    Ok(BoundedAttachmentPreview {
        kind: AttachmentPreviewKind::Binary,
        mime_type: "application/octet-stream".into(),
        byte_size: bytes.len(),
        width: None,
        height: None,
        pdf_pages_estimate: None,
        text_snippet: None,
        truncated: false,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn user_attachment_import_is_unique_and_rejects_special_paths() {
        let root = std::env::temp_dir().join(format!("gm-attachment-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let config = gm_core::config::GmConfig {
            storage_dir: root.join("data"),
            ..Default::default()
        };
        let (tx, _) = tokio::sync::broadcast::channel(8);
        let supervisor = OpenBotSupervisor::open(
            config,
            std::sync::Arc::new(gm_safety::AuditLogger::in_memory()),
            tx,
        )
        .unwrap();
        let source = root.join("report.txt");
        std::fs::write(&source, b"user report").unwrap();
        let first = supervisor.import_attachment("researcher", &source).unwrap();
        let second = supervisor.import_attachment("researcher", &source).unwrap();
        assert_ne!(first, second);
        let sandbox = supervisor.roster().workspace_sandbox("researcher").unwrap();
        assert_eq!(sandbox.preview_shared_file(&first).unwrap(), b"user report");
        assert!(sandbox.preview_shared_file("outbox/../../escape").is_err());
        assert_eq!(sandbox.shared_files().unwrap().len(), 2);
        assert!(supervisor.import_attachment("unknown", &source).is_err());
        assert!(supervisor.import_attachment("researcher", &root).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&source, root.join("link")).unwrap();
            assert!(
                supervisor
                    .import_attachment("researcher", &root.join("link"))
                    .is_err()
            );
        }
        drop(sandbox);
        drop(supervisor);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn bounded_passive_image_and_pdf_previews() {
        let pdf_bytes = b"%PDF-1.7\n1 0 obj << /Type /Catalog >> endobj\n2 0 obj << /Type /Page >> endobj\n(Quarterly Summary) Tj";
        let pdf_prev = preview_attachment_bounded("report.pdf", pdf_bytes).unwrap();
        assert_eq!(pdf_prev.kind, AttachmentPreviewKind::Pdf);
        assert_eq!(pdf_prev.pdf_pages_estimate, Some(1));
        assert!(pdf_prev.text_snippet.unwrap().contains("Quarterly Summary"));

        let img = image::RgbaImage::new(32, 24);
        let mut png_bytes = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut png_bytes),
            image::ImageFormat::Png,
        )
        .unwrap();
        let img_prev = preview_attachment_bounded("shot.png", &png_bytes).unwrap();
        assert_eq!(img_prev.kind, AttachmentPreviewKind::Image);
        assert_eq!((img_prev.width, img_prev.height), (Some(32), Some(24)));

        let oversized = vec![b'a'; MAX_PREVIEW_INPUT_BYTES + 1];
        assert!(preview_attachment_bounded("huge.txt", &oversized).is_err());
    }
}
