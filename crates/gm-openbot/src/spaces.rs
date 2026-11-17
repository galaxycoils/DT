//! Static Spaces runtime (`T5.2`) served strictly on a separate HTTP origin
//! (`127.0.0.1:<spaces_port or companion_port+1>`) with ephemeral HMAC-SHA256
//! capability tokens, strict CSP/nosniff/no-store headers, and Static-only scaffolding.

use axum::Router;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use chrono::Utc;
use gm_core::config::MuseConfig;
use gm_core::error::{GmError, Result};
use gm_core::muse_paths::{MountAccess, MuseRuntimePaths};
use gm_core::storage::ensure_private_dir;
use gm_muse::shared_files::SharedFileStore;
use gm_safety::{AuditCategory, AuditLogger};
use hmac::{Hmac, Mac};
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::net::SocketAddr;
use std::path::{Component, PathBuf};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, OnceLock};
use tokio::net::TcpListener;
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

/// Capability token lifetime (15 minutes).
pub const SPACE_TOKEN_TTL_SECS: i64 = 15 * 60;

fn slug_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[a-z0-9][a-z0-9-]{0,63}$").expect("valid space slug regex"))
}

pub fn validate_space_slug(slug: &str) -> Result<()> {
    if !slug_regex().is_match(slug) {
        return Err(GmError::SafetyViolation(format!(
            "RULE-PATH-002: Invalid Space slug '{slug}' (expected ^[a-z0-9][a-z0-9-]{{0,63}}$)"
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpaceKind {
    Static,
    Ts,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceMetadata {
    pub slug: String,
    pub title: String,
    pub kind: SpaceKind,
    pub published: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// Generate a fresh 32-byte ephemeral secret held strictly in memory for the daemon lifetime.
pub fn generate_ephemeral_space_secret() -> [u8; 32] {
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    let mut out = [0u8; 32];
    out[..16].copy_from_slice(a.as_bytes());
    out[16..].copy_from_slice(b.as_bytes());
    out
}

/// Mint a short-lived space capability token `"<exp>.<hex(hmac_sha256(secret, slug|exp))>"`.
pub fn mint_space_token(secret: &[u8], slug: &str, exp_unix: i64) -> String {
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC takes any key size");
    mac.update(format!("{slug}|{exp_unix}").as_bytes());
    let tag = hex::encode(mac.finalize().into_bytes());
    format!("{exp_unix}.{tag}")
}

/// Verify a space capability token in constant time (`hmac::Mac::verify_slice`) and check expiry.
pub fn verify_space_token(secret: &[u8], slug: &str, token: &str, now_unix: i64) -> bool {
    if validate_space_slug(slug).is_err() {
        return false;
    }
    let Some((exp_str, sig_hex)) = token.split_once('.') else {
        return false;
    };
    if exp_str.is_empty() || !exp_str.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let Ok(exp_unix) = exp_str.parse::<i64>() else {
        return false;
    };
    if exp_unix < now_unix {
        return false;
    }
    let Ok(sig_bytes) = hex::decode(sig_hex) else {
        return false;
    };
    let Ok(mut mac) = HmacSha256::new_from_slice(secret) else {
        return false;
    };
    mac.update(format!("{slug}|{exp_unix}").as_bytes());
    mac.verify_slice(&sig_bytes).is_ok()
}

pub fn default_static_space_html(slug: &str, title: &str, companion_port: u16) -> String {
    let escaped_title = title
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;");
    let escaped_slug = slug
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;");
    let expected_origin = format!("http://127.0.0.1:{companion_port}");
    format!(
        r#"<!DOCTYPE html>
<html lang="en" data-theme="dark">
<head>
<meta charset="utf-8" />
<meta name="viewport" content="width=device-width,initial-scale=1" />
<title>{escaped_title}</title>
<style>
  :root {{
    --bg: #0b0f17;
    --fg: #e6edf7;
    --card: #131b2e;
    --border: #23304d;
    --accent: #4f8cff;
  }}
  :root[data-theme="light"] {{
    --bg: #f6f8fb;
    --fg: #141c2c;
    --card: #ffffff;
    --border: #d7e0ee;
    --accent: #2563eb;
  }}
  body {{
    margin: 0;
    padding: 20px;
    font-family: -apple-system, BlinkMacSystemFont, system-ui, sans-serif;
    background: var(--bg);
    color: var(--fg);
  }}
  .space-card {{
    background: var(--card);
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 16px;
    max-width: 720px;
  }}
  h1 {{ margin: 0 0 8px 0; font-size: 18px; }}
  p {{ margin: 0; opacity: 0.85; }}
</style>
</head>
<body>
  <div class="space-card">
    <h1>{escaped_title}</h1>
    <p>Static Muse Space: <code>{escaped_slug}</code></p>
  </div>
<script>
(function () {{
  var allowedOrigin = "{expected_origin}";
  window.addEventListener("message", function (event) {{
    if (event.origin !== allowedOrigin && event.origin !== "null") return;
    var payload = event && event.data;
    if (!payload || payload.type !== "hatch:space:theme") return;
    if (payload.theme === "dark" || payload.theme === "light") {{
      document.documentElement.dataset.theme = payload.theme;
    }}
  }});
}})();
</script>
</body>
</html>
"#
    )
}

#[derive(Clone)]
pub struct SpacesManager {
    paths: MuseRuntimePaths,
    secret: Arc<[u8; 32]>,
    audit: Arc<AuditLogger>,
    muse_config: MuseConfig,
    companion_port: Arc<AtomicU16>,
}

impl SpacesManager {
    pub fn new(
        paths: MuseRuntimePaths,
        audit: Arc<AuditLogger>,
        muse_config: MuseConfig,
        companion_port: u16,
    ) -> Self {
        Self {
            paths,
            secret: Arc::new(generate_ephemeral_space_secret()),
            audit,
            muse_config,
            companion_port: Arc::new(AtomicU16::new(companion_port)),
        }
    }

    pub fn with_shared_secret(mut self, secret: [u8; 32]) -> Self {
        self.secret = Arc::new(secret);
        self
    }

    pub fn set_companion_port(&self, port: u16) {
        self.companion_port.store(port, Ordering::SeqCst);
    }

    pub fn companion_port(&self) -> u16 {
        self.companion_port.load(Ordering::SeqCst)
    }

    pub fn secret(&self) -> &[u8] {
        self.secret.as_ref()
    }

    pub fn spaces_root(&self) -> PathBuf {
        self.paths.spaces_dir()
    }

    pub fn space_dir(&self, slug: &str) -> Result<PathBuf> {
        validate_space_slug(slug)?;
        Ok(self.paths.spaces_dir().join(slug))
    }

    pub fn mint_url_for_slug(&self, slug: &str, spaces_port: u16) -> Result<String> {
        validate_space_slug(slug)?;
        let dir = self.space_dir(slug)?;
        if !dir.join("space.json").exists() && !dir.join("index.html").exists() {
            return Err(GmError::Config(format!("Space '{slug}' does not exist")));
        }
        let exp = Utc::now().timestamp() + SPACE_TOKEN_TTL_SECS;
        let tok = mint_space_token(self.secret(), slug, exp);
        Ok(format!(
            "http://127.0.0.1:{spaces_port}/spaces/v2/{slug}/?token={tok}"
        ))
    }

    pub fn create_space(
        &self,
        slug: &str,
        title: Option<&str>,
        kind: SpaceKind,
        initial_html: Option<&str>,
    ) -> Result<SpaceMetadata> {
        if kind == SpaceKind::Ts {
            return Err(GmError::Config("TS Spaces are not supported yet".into()));
        }
        validate_space_slug(slug)?;
        let dir = self.space_dir(slug)?;
        ensure_private_dir(&self.paths.spaces_dir())?;
        ensure_private_dir(&dir)?;

        let clean_title = title
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(slug)
            .to_string();
        let now = Utc::now().to_rfc3339();
        let meta = SpaceMetadata {
            slug: slug.to_string(),
            title: clean_title.clone(),
            kind: SpaceKind::Static,
            published: false,
            created_at: now.clone(),
            updated_at: now,
        };

        let meta_path = dir.join("space.json");
        let meta_json = serde_json::to_string_pretty(&meta)?;
        SharedFileStore::global().write_exact_sync(&meta_path, None, &meta_json)?;

        let html = initial_html.map(str::to_string).unwrap_or_else(|| {
            default_static_space_html(slug, &clean_title, self.companion_port())
        });
        let index_path = dir.join("index.html");
        SharedFileStore::global().write_exact_sync(&index_path, None, &html)?;

        Ok(meta)
    }

    pub fn update_space_file(&self, slug: &str, rel_path: &str, content: &str) -> Result<PathBuf> {
        validate_space_slug(slug)?;
        let clean = normalize_rel_space_path(rel_path)?;
        let resolved = self
            .paths
            .resolve_virtual_path(&format!("spaces/{slug}/{clean}"))?;
        if resolved.access == MountAccess::ReadOnly {
            return Err(GmError::SafetyViolation(
                "RULE-PATH-003: Space mount cannot be read-only".into(),
            ));
        }
        if let Some(parent) = resolved.absolute.parent() {
            ensure_private_dir(parent)?;
        }
        SharedFileStore::global().write_exact_sync(&resolved.absolute, None, content)?;
        if let Ok(Some(mut meta)) = self.get_space(slug) {
            meta.updated_at = Utc::now().to_rfc3339();
            let meta_json = serde_json::to_string_pretty(&meta)?;
            let _ = SharedFileStore::global().write_exact_sync(
                self.space_dir(slug)?.join("space.json"),
                None,
                &meta_json,
            );
        }
        Ok(resolved.absolute)
    }

    pub fn get_space(&self, slug: &str) -> Result<Option<SpaceMetadata>> {
        validate_space_slug(slug)?;
        let dir = self.space_dir(slug)?;
        let meta_path = dir.join("space.json");
        if let Some((raw, _)) = SharedFileStore::global().read_text_with_sha256(&meta_path)? {
            let meta: SpaceMetadata = serde_json::from_str(&raw)?;
            return Ok(Some(meta));
        }
        if dir.join("index.html").exists() {
            let now = Utc::now().to_rfc3339();
            return Ok(Some(SpaceMetadata {
                slug: slug.to_string(),
                title: slug.to_string(),
                kind: SpaceKind::Static,
                published: false,
                created_at: now.clone(),
                updated_at: now,
            }));
        }
        Ok(None)
    }

    pub fn list_spaces(&self) -> Result<Vec<SpaceMetadata>> {
        let root = self.paths.spaces_dir();
        if !root.exists() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&root)? {
            let entry = entry?;
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.file_type().is_symlink() || !meta.is_dir() {
                continue;
            }
            let Some(slug) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if validate_space_slug(slug).is_err() {
                continue;
            }
            if let Ok(Some(info)) = self.get_space(slug) {
                out.push(info);
            }
        }
        out.sort_by(|a, b| a.slug.cmp(&b.slug));
        Ok(out)
    }

    pub fn publish_space(&self, slug: &str, confirmed: bool) -> Result<SpaceMetadata> {
        let require_confirm = self.muse_config.artifact_require_publish_confirmation;
        if require_confirm && !confirmed {
            self.audit.log_governance(
                "space_publish_refused",
                Some("muse"),
                AuditCategory::Refused,
                "RULE-TIER-003",
                "Publishing or sharing a Space is an Irreversible action requiring confirmation",
                format!("Refused unconfirmed publish for Space '{slug}'"),
                serde_json::json!({ "slug": slug }),
            );
            return Err(GmError::SafetyViolation(format!(
                "RULE-TIER-003: Publishing Space '{slug}' is Irreversible and requires explicit confirmation"
            )));
        }
        let mut meta = self
            .get_space(slug)?
            .ok_or_else(|| GmError::Config(format!("Space '{slug}' not found")))?;
        meta.published = true;
        meta.updated_at = Utc::now().to_rfc3339();
        let meta_path = self.space_dir(slug)?.join("space.json");
        let meta_json = serde_json::to_string_pretty(&meta)?;
        SharedFileStore::global().write_exact_sync(&meta_path, None, &meta_json)?;
        self.audit.log_governance(
            "space_published",
            Some("muse"),
            AuditCategory::Permitted,
            "RULE-TIER-003",
            "Confirmed Irreversible publish of static Space",
            format!("Published Space '{slug}'"),
            serde_json::json!({ "slug": slug }),
        );
        Ok(meta)
    }

    pub fn unpublish_space(&self, slug: &str) -> Result<SpaceMetadata> {
        let mut meta = self
            .get_space(slug)?
            .ok_or_else(|| GmError::Config(format!("Space '{slug}' not found")))?;
        meta.published = false;
        meta.updated_at = Utc::now().to_rfc3339();
        let meta_path = self.space_dir(slug)?.join("space.json");
        let meta_json = serde_json::to_string_pretty(&meta)?;
        SharedFileStore::global().write_exact_sync(&meta_path, None, &meta_json)?;
        Ok(meta)
    }

    pub fn delete_space(&self, slug: &str, confirmed: bool) -> Result<bool> {
        validate_space_slug(slug)?;
        if !confirmed {
            self.audit.log_governance(
                "space_delete_refused",
                Some("muse"),
                AuditCategory::Refused,
                "RULE-TIER-003",
                "Deleting a Space is Irreversible",
                format!("Refused unconfirmed delete for Space '{slug}'"),
                serde_json::json!({ "slug": slug }),
            );
            return Err(GmError::SafetyViolation(format!(
                "RULE-TIER-003: Deleting Space '{slug}' requires explicit confirmation"
            )));
        }
        let dir = self.space_dir(slug)?;
        if !dir.exists() {
            return Ok(false);
        }
        let meta = std::fs::symlink_metadata(&dir)?;
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return Err(GmError::SafetyViolation(
                "RULE-PATH-004: Refusing to delete symlinked Space directory".into(),
            ));
        }
        std::fs::remove_dir_all(&dir)?;
        self.audit.log_governance(
            "space_deleted",
            Some("muse"),
            AuditCategory::Permitted,
            "RULE-TIER-003",
            "Confirmed deletion of Space",
            format!("Deleted Space '{slug}'"),
            serde_json::json!({ "slug": slug }),
        );
        Ok(true)
    }

    pub fn resolve_asset_file(&self, slug: &str, rel_path: &str) -> Result<PathBuf> {
        validate_space_slug(slug)?;
        let clean = normalize_rel_space_path(rel_path)?;
        let resolved = self
            .paths
            .resolve_virtual_path(&format!("spaces/{slug}/{clean}"))?;
        let meta = std::fs::symlink_metadata(&resolved.absolute)
            .map_err(|_| GmError::Config("Space asset not found".into()))?;
        if meta.file_type().is_symlink() || !meta.is_file() {
            return Err(GmError::SafetyViolation(
                "RULE-PATH-004: Space asset missing or is a symlink".into(),
            ));
        }
        Ok(resolved.absolute)
    }
}

fn normalize_rel_space_path(raw: &str) -> Result<String> {
    let trimmed = raw.trim().trim_start_matches('/');
    if trimmed.is_empty()
        || trimmed.contains('\0')
        || trimmed.contains('\\')
        || trimmed.contains("..")
    {
        return Err(GmError::SafetyViolation(format!(
            "RULE-PATH-002: Invalid Space relative path '{raw}'"
        )));
    }
    let mut parts = Vec::new();
    for comp in std::path::Path::new(trimmed).components() {
        match comp {
            Component::Normal(s) => {
                let st = s.to_str().ok_or_else(|| {
                    GmError::SafetyViolation("RULE-PATH-002: Non-UTF8 path".into())
                })?;
                if st == ".muse.lock" {
                    return Err(GmError::SafetyViolation(
                        "RULE-PATH-002: Lock file cannot be served".into(),
                    ));
                }
                parts.push(st.to_string());
            }
            _ => {
                return Err(GmError::SafetyViolation(format!(
                    "RULE-PATH-002: Traversal component in Space path '{raw}'"
                )));
            }
        }
    }
    if parts.is_empty() {
        return Err(GmError::SafetyViolation(
            "RULE-PATH-002: Empty Space path".into(),
        ));
    }
    Ok(parts.join("/"))
}

fn content_type_for_path(path: &std::path::Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("txt" | "md") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[derive(Clone)]
pub struct SpacesWebState {
    pub spaces: SpacesManager,
}

/// Separate axum router exclusively for static Spaces on its own loopback port.
/// Registers ONLY `GET /spaces/v2/:slug/:token/*path` (404 elsewhere, 405 non-GET).
pub fn build_spaces_router(state: SpacesWebState) -> Router {
    let companion_port_atomic = Arc::clone(&state.spaces.companion_port);
    Router::new()
        .route("/spaces/v2/:slug/", get(open_space_query))
        .route("/spaces/v2/:slug/:token/*path", get(serve_space_asset))
        .layer(axum::middleware::from_fn(
            move |request: axum::extract::Request, next: axum::middleware::Next| {
                let comp_port_ref = Arc::clone(&companion_port_atomic);
                async move {
                    let mut response = next.run(request).await;
                    let comp_port = comp_port_ref.load(Ordering::SeqCst);
                    let csp = format!(
                        "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; connect-src 'self'; img-src 'self' data: blob:; frame-ancestors http://127.0.0.1:{comp_port}; base-uri 'none'; object-src 'none'"
                    );
                    if let Ok(csp_val) = HeaderValue::from_str(&csp) {
                        response
                            .headers_mut()
                            .insert(header::CONTENT_SECURITY_POLICY, csp_val);
                    }
                    response.headers_mut().insert(
                        header::X_CONTENT_TYPE_OPTIONS,
                        HeaderValue::from_static("nosniff"),
                    );
                    response.headers_mut().insert(
                        header::CACHE_CONTROL,
                        HeaderValue::from_static("no-store"),
                    );
                    response.headers_mut().insert(
                        header::REFERRER_POLICY,
                        HeaderValue::from_static("no-referrer"),
                    );
                    response
                }
            },
        ))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .with_state(state)
}

// The canonical query-token entry redirects into the capability directory so
// relative CSS/JS assets retain authorization without cookies or shared origin.
async fn open_space_query(
    Path(slug): Path<String>,
    axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>,
    State(state): State<SpacesWebState>,
) -> axum::response::Response {
    if validate_space_slug(&slug).is_err() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let token = query.get("token").map(String::as_str).unwrap_or_default();
    if !verify_space_token(state.spaces.secret(), &slug, token, Utc::now().timestamp()) {
        return StatusCode::FORBIDDEN.into_response();
    }
    axum::response::Redirect::temporary(&format!("/spaces/v2/{slug}/{token}/index.html"))
        .into_response()
}

async fn serve_space_asset(
    Path((slug, token, rel_path)): Path<(String, String, String)>,
    State(state): State<SpacesWebState>,
) -> impl IntoResponse {
    // Reject bad slugs and path traversals (`..`, NUL, backslash) with 404 before auth oracle leaks.
    if validate_space_slug(&slug).is_err() || normalize_rel_space_path(&rel_path).is_err() {
        return StatusCode::NOT_FOUND.into_response();
    }

    let now = Utc::now().timestamp();
    if !verify_space_token(state.spaces.secret(), &slug, &token, now) {
        return StatusCode::FORBIDDEN.into_response();
    }

    let Ok(abs) = state.spaces.resolve_asset_file(&slug, &rel_path) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let Ok(bytes) = tokio::fs::read(&abs).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let ctype = content_type_for_path(&abs);
    ([(header::CONTENT_TYPE, ctype)], bytes).into_response()
}

pub struct SpacesWebServer {
    state: SpacesWebState,
}

impl SpacesWebServer {
    pub fn new(spaces: SpacesManager) -> Self {
        Self {
            state: SpacesWebState { spaces },
        }
    }

    pub async fn bind_and_serve(
        self,
        bind_addr: &str,
    ) -> Result<(SocketAddr, tokio::task::JoinHandle<()>)> {
        let requested: SocketAddr = bind_addr.parse().map_err(|_| {
            GmError::Config("Spaces server requires an explicit loopback IP and port".into())
        })?;
        if !requested.ip().is_loopback() {
            return Err(GmError::Config(
                "Spaces server must bind only to a loopback address".into(),
            ));
        }
        let listener = TcpListener::bind(requested).await?;
        let local_addr = listener.local_addr()?;
        let router = build_spaces_router(self.state);
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Ok((local_addr, handle))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_manager(dir: &std::path::Path) -> SpacesManager {
        let paths = MuseRuntimePaths::shared(dir);
        SpacesManager::new(
            paths,
            Arc::new(AuditLogger::in_memory()),
            MuseConfig::default(),
            7433,
        )
    }

    #[tokio::test]
    async fn spaces_router_enforces_token_traversal_method_headers_and_static_only() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = test_manager(dir.path());

        // 1. `kind: ts` MUST be rejected
        let ts_err = mgr
            .create_space("ts-app", Some("TS App"), SpaceKind::Ts, None)
            .unwrap_err();
        assert!(
            ts_err
                .to_string()
                .contains("TS Spaces are not supported yet")
        );

        // 2. Publish requires Irreversible confirmation when artifact_require_publish_confirmation = true
        let created = mgr
            .create_space(
                "habit-tracker",
                Some("Habit Tracker"),
                SpaceKind::Static,
                None,
            )
            .unwrap();
        assert!(!created.published);
        assert!(mgr.publish_space("habit-tracker", false).is_err());
        let published = mgr.publish_space("habit-tracker", true).unwrap();
        assert!(published.published);

        let server = SpacesWebServer::new(mgr.clone());
        let (addr, handle) = server.bind_and_serve("127.0.0.1:0").await.unwrap();
        let client = reqwest::Client::new();
        let base = format!("http://{addr}");

        let now = Utc::now().timestamp();
        let valid_tok = mint_space_token(mgr.secret(), "habit-tracker", now + 600);
        let expired_tok = mint_space_token(mgr.secret(), "habit-tracker", now - 60);
        let other_slug_tok = mint_space_token(mgr.secret(), "other-space", now + 600);

        // Valid request -> 200 with strict CSP + nosniff + no-store + hatch:space:theme listener
        let ok = client
            .get(format!(
                "{base}/spaces/v2/habit-tracker/{valid_tok}/index.html"
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(ok.status(), 200);
        assert_eq!(ok.headers()["x-content-type-options"], "nosniff");
        assert_eq!(ok.headers()["cache-control"], "no-store");
        assert_eq!(ok.headers()["referrer-policy"], "no-referrer");
        let csp = ok.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .to_string();
        assert!(csp.contains("frame-ancestors http://127.0.0.1:7433"));
        let body = ok.text().await.unwrap();
        assert!(body.contains("hatch:space:theme"));

        // Expired token or wrong slug token -> 403
        let expired = client
            .get(format!(
                "{base}/spaces/v2/habit-tracker/{expired_tok}/index.html"
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(expired.status(), 403);

        let wrong_slug = client
            .get(format!(
                "{base}/spaces/v2/habit-tracker/{other_slug_tok}/index.html"
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(wrong_slug.status(), 403);

        // `..` path traversal (URL-encoded so reqwest sends it verbatim to server) -> 404
        let traversal = client
            .get(format!(
                "{base}/spaces/v2/habit-tracker/{valid_tok}/..%2fUSER.md"
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(traversal.status(), 404);

        // Non-GET (POST) on Space route -> 405 Method Not Allowed
        let post = client
            .post(format!(
                "{base}/spaces/v2/habit-tracker/{valid_tok}/index.html"
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(post.status(), 405);

        // Unrelated routes on Spaces origin -> 404
        let api_probe = client
            .get(format!("{base}/api/runtime"))
            .send()
            .await
            .unwrap();
        assert_eq!(api_probe.status(), 404);

        handle.abort();
    }
}
