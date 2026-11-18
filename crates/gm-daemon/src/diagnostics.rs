//! Local system and agent health diagnostics and redacted support bundle export.
//!
//! Scrubs all credentials, tokens, and personal secrets before writing support bundles.

use chrono::Utc;
use gm_core::config::GmConfig;
use gm_core::error::Result;
use gm_platform::permissions::{PrivacyPane, check_all_permissions};
use gm_safety::SecretScrubber;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiagnosticStatus {
    Ok,
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticCheck {
    pub name: String,
    pub category: String,
    pub status: DiagnosticStatus,
    pub message: String,
    pub remediation: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthReport {
    pub overall: DiagnosticStatus,
    pub timestamp: String,
    pub checks: Vec<DiagnosticCheck>,
}

impl HealthReport {
    pub fn is_healthy(&self) -> bool {
        self.overall == DiagnosticStatus::Ok
    }
}

/// Run comprehensive local diagnostics across sockets, storage, permissions, and models.
pub fn run_health_diagnostics(config: &GmConfig) -> HealthReport {
    let mut checks = Vec::new();

    // 1. Storage directory integrity (omit identifying absolute paths in exported reports)
    let storage_dir = &config.storage_dir;
    if storage_dir.exists() {
        let probe_file = storage_dir.join(format!(".diag-probe-{}", uuid::Uuid::new_v4()));
        match std::fs::write(&probe_file, b"ok") {
            Ok(()) => {
                let _ = std::fs::remove_file(probe_file);
                checks.push(DiagnosticCheck {
                    name: "Storage Directory".into(),
                    category: "Filesystem".into(),
                    status: DiagnosticStatus::Ok,
                    message: "Configured storage directory is writable".into(),
                    remediation: None,
                });
            }
            Err(e) => {
                checks.push(DiagnosticCheck {
                    name: "Storage Directory".into(),
                    category: "Filesystem".into(),
                    status: DiagnosticStatus::Error,
                    message: format!("Configured storage directory is not writable: {e}"),
                    remediation: Some("Check filesystem permissions for the storage path".into()),
                });
            }
        }
    } else {
        checks.push(DiagnosticCheck {
            name: "Storage Directory".into(),
            category: "Filesystem".into(),
            status: DiagnosticStatus::Warning,
            message: "Configured storage directory does not exist yet (will be created on startup)"
                .into(),
            remediation: None,
        });
    }

    // 2. OpenBot Queue database integrity
    let queue_path = storage_dir.join("openbot.sqlite");
    if queue_path.exists() {
        match gm_openbot::check_sqlite_integrity(&queue_path) {
            Ok(_) => {
                checks.push(DiagnosticCheck {
                    name: "OpenBot Task Queue".into(),
                    category: "Database".into(),
                    status: DiagnosticStatus::Ok,
                    message: "SQLite task queue database is healthy".into(),
                    remediation: None,
                });
            }
            Err(e) => {
                checks.push(DiagnosticCheck {
                    name: "OpenBot Task Queue".into(),
                    category: "Database".into(),
                    status: DiagnosticStatus::Error,
                    message: format!("Task queue database check failed: {e}"),
                    remediation: Some("Run 'gm queue restore' or inspect backup files".into()),
                });
            }
        }
    } else {
        checks.push(DiagnosticCheck {
            name: "OpenBot Task Queue".into(),
            category: "Database".into(),
            status: DiagnosticStatus::Ok,
            message: "Task queue database will be created on first task".into(),
            remediation: None,
        });
    }

    // 3. Channels database integrity
    let channels_path = storage_dir.join("channels.sqlite");
    if channels_path.exists() {
        match gm_openbot::check_sqlite_integrity(&channels_path) {
            Ok(_) => {
                checks.push(DiagnosticCheck {
                    name: "Channels Store".into(),
                    category: "Database".into(),
                    status: DiagnosticStatus::Ok,
                    message: "SQLite channels database is healthy".into(),
                    remediation: None,
                });
            }
            Err(e) => {
                checks.push(DiagnosticCheck {
                    name: "Channels Store".into(),
                    category: "Database".into(),
                    status: DiagnosticStatus::Error,
                    message: format!("Channels database check failed: {e}"),
                    remediation: Some("Inspect channel database backups".into()),
                });
            }
        }
    }

    // 4. macOS / Platform Permissions
    let perms = check_all_permissions();
    let perm_items = [
        (
            "Screen Recording",
            PrivacyPane::ScreenRecording,
            perms.screen_recording,
        ),
        (
            "Accessibility",
            PrivacyPane::Accessibility,
            perms.accessibility,
        ),
        ("Calendar", PrivacyPane::Calendars, perms.calendar),
        ("Reminders", PrivacyPane::Reminders, perms.reminders),
    ];
    for (name, pane, is_granted) in perm_items {
        checks.push(DiagnosticCheck {
            name: format!("{name} Permission"),
            category: "Permissions".into(),
            status: if is_granted {
                DiagnosticStatus::Ok
            } else {
                DiagnosticStatus::Warning
            },
            message: format!("Permission {name}: granted={is_granted}"),
            remediation: if is_granted {
                None
            } else {
                Some(format!(
                    "Open macOS System Settings via '{}' or 'gm permissions --open {}'",
                    pane.url(),
                    pane.pane_name()
                ))
            },
        });
    }

    // 5. Helper IPC Socket
    let socket_path = &config.socket_path;
    let socket_exists = socket_path.exists();
    checks.push(DiagnosticCheck {
        name: "CU-Helper Socket".into(),
        category: "IPC".into(),
        status: if socket_exists {
            DiagnosticStatus::Ok
        } else {
            DiagnosticStatus::Warning
        },
        message: if socket_exists {
            "Helper IPC endpoint is present".into()
        } else {
            "Helper IPC endpoint is not currently active (daemon auto-spawns it on demand)".into()
        },
        remediation: None,
    });

    // Determine overall status
    let mut overall = DiagnosticStatus::Ok;
    for c in &checks {
        match c.status {
            DiagnosticStatus::Error => {
                overall = DiagnosticStatus::Error;
                break;
            }
            DiagnosticStatus::Warning if overall != DiagnosticStatus::Error => {
                overall = DiagnosticStatus::Warning;
            }
            _ => {}
        }
    }

    HealthReport {
        overall,
        timestamp: Utc::now().to_rfc3339(),
        checks,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupportExportBundle {
    pub export_timestamp: String,
    pub grok_muse_version: String,
    pub os: String,
    pub arch: String,
    pub health: HealthReport,
    pub sanitized_config: serde_json::Value,
    pub redaction_verified: bool,
    pub scrubbed_secret_count: usize,
    pub sha256_fingerprint: String,
}

fn allowlisted_config_summary(config: &GmConfig) -> serde_json::Value {
    let profiles: Vec<serde_json::Value> = config
        .providers
        .profiles
        .iter()
        .map(|p| {
            serde_json::json!({
                "id": p.id,
                "preset": p.preset,
                "protocol": p.protocol,
                "auth_mode": p.auth_mode,
                "selected_model": p.selected_model,
                "allow_screenshot_egress": p.allow_screenshot_egress,
                "connection_state": p.connection_state,
                "verification_status": p.health.verification_status,
                "catalog_status": p.health.catalog_status,
            })
        })
        .collect();
    serde_json::json!({
        "providers": {
            "workspace_default_profile_id": config.providers.workspace_default_profile_id,
            "account_default_profile_id": config.providers.account_default_profile_id,
            "allow_screenshot_egress_meta": config.providers.allow_screenshot_egress_meta,
            "allow_screenshot_egress_xai": config.providers.allow_screenshot_egress_xai,
            "profiles": profiles,
        },
        "safety": {
            "allow_host_desktop": config.safety.allow_host_desktop,
            "allow_secret_injection": config.safety.allow_secret_injection,
            "redact_secure_fields": config.safety.redact_secure_fields,
            "blocklisted_apps_count": config.safety.blocklisted_apps.len(),
            "user_active_pause_seconds": config.safety.user_active_pause_seconds,
        },
        "budget": {
            "daily_spend_cap_usd": config.budget.daily_spend_cap_usd,
            "per_job_spend_cap_usd": config.budget.per_job_spend_cap_usd,
        },
        "muse": {
            "context_tokens": config.muse.context_tokens,
            "heartbeat_interval_secs": config.muse.heartbeat_interval_secs,
            "enable_desktop_patrol": config.muse.enable_desktop_patrol,
        },
        "ui": {
            "theme": config.ui.theme,
            "accent_mode": config.ui.accent_mode,
            "computer_execution_mode": config.ui.computer_execution_mode,
        },
        "openbot": {
            "companion_enabled": config.openbot.companion_enabled,
            "max_handoff_depth": config.openbot.max_handoff_depth,
        },
    })
}

fn find_unredacted_reason(bytes: &[u8], forbidden_literals: &[(String, String)]) -> Option<String> {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Some("non-utf8".into());
    };
    for (label, secret) in forbidden_literals {
        if !secret.is_empty() && text.contains(secret) {
            return Some(format!("literal:{label}"));
        }
    }
    for prefix in ["sk-proj-", "sk-ant-", "ghp_", "gho_", "Bearer "] {
        if text.contains(prefix) {
            return Some(format!("prefix:{prefix}"));
        }
    }
    for (idx, _) in text.match_indices("xai-") {
        let rest = &text[idx..];
        let token: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        if token != "xai-default" && token.len() >= 16 {
            return Some("prefix:xai-".into());
        }
    }
    None
}

/// Build an allowlisted, redacted support bundle preview in memory and verify the final serialized bytes.
pub fn preview_support_bundle(config: &GmConfig) -> Result<SupportExportBundle> {
    let mut health = run_health_diagnostics(config);

    let scrubber = SecretScrubber::new();
    let mut secret_count = 0usize;
    let mut forbidden_literals: Vec<(String, String)> = Vec::new();

    for (k, v) in std::env::vars() {
        let upper = k.to_uppercase();
        let is_sensitive_key = upper.ends_with("_KEY")
            || upper.ends_with("_TOKEN")
            || upper.ends_with("_SECRET")
            || upper.ends_with("_PASSWORD")
            || upper.ends_with("_AUTH")
            || upper.contains("API_KEY")
            || upper.contains("AUTH_TOKEN");
        if is_sensitive_key && !v.trim().is_empty() && v.len() >= 12 {
            let label = format!("ENV_{upper}");
            scrubber.register(&label, &v);
            forbidden_literals.push((label, v));
            secret_count += 1;
        }
    }

    for path in [
        &config.storage_dir,
        &config.socket_path,
        &config.daemon_socket_path,
        &config.openbot.bots_dir,
    ] {
        let s = path.display().to_string();
        if s.len() >= 6 && (s.contains('/') || s.contains('\\')) {
            scrubber.register("PATH_LOCAL", &s);
            forbidden_literals.push(("PATH_LOCAL".into(), s));
        }
    }
    if let Ok(home) = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE"))
        && home.len() >= 4
    {
        scrubber.register("HOME_DIR", &home);
        forbidden_literals.push(("HOME_DIR".into(), home));
    }

    for check in &mut health.checks {
        check.message = scrubber.scrub_str(&check.message);
        if let Some(ref rem) = check.remediation {
            check.remediation = Some(scrubber.scrub_str(rem));
        }
    }

    let sanitized_config = scrubber.scrub_json(&allowlisted_config_summary(config));

    let mut bundle = SupportExportBundle {
        export_timestamp: Utc::now().to_rfc3339(),
        grok_muse_version: env!("CARGO_PKG_VERSION").to_string(),
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        health,
        sanitized_config,
        redaction_verified: false,
        scrubbed_secret_count: secret_count,
        sha256_fingerprint: String::new(),
    };

    let candidate_bytes = serde_json::to_vec_pretty(&bundle)?;
    if let Some(reason) = find_unredacted_reason(&candidate_bytes, &forbidden_literals) {
        return Err(gm_core::error::GmError::SafetyViolation(format!(
            "Support export byte verification detected an unredacted secret or identifying path ({reason})"
        )));
    }

    bundle.redaction_verified = true;
    let fingerprint_input = serde_json::to_string_pretty(&bundle)?;
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(fingerprint_input.as_bytes());
    bundle.sha256_fingerprint = hex::encode(hasher.finalize());

    let final_bytes = serde_json::to_vec_pretty(&bundle)?;
    if let Some(reason) = find_unredacted_reason(&final_bytes, &forbidden_literals) {
        return Err(gm_core::error::GmError::SafetyViolation(format!(
            "Support export final byte verification failed ({reason})"
        )));
    }

    Ok(bundle)
}

/// Create an explicitly approved, redacted diagnostic support export scrubbing all secrets.
pub fn export_support_bundle(config: &GmConfig, output_path: &Path) -> Result<SupportExportBundle> {
    let bundle = preview_support_bundle(config)?;
    let final_bytes = serde_json::to_vec_pretty(&bundle)?;
    gm_core::storage::atomic_write_private(output_path, &final_bytes)?;
    Ok(bundle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_health_diagnostics_runs_cleanly() {
        let dir = std::env::temp_dir().join(format!("gm-diag-test-{}", uuid::Uuid::new_v4()));
        let config = GmConfig {
            storage_dir: dir.clone(),
            ..Default::default()
        };

        let report = run_health_diagnostics(&config);
        assert!(!report.checks.is_empty());
        assert!(report.checks.iter().any(|c| c.name == "Storage Directory"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_support_bundle_redaction_allowlist_and_fingerprint() {
        let dir = std::env::temp_dir().join(format!("gm-export-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let export_path = dir.join("support_bundle.json");

        unsafe {
            std::env::set_var("TEST_DUMMY_SECRET_KEY", "super-confidential-token-12345");
            std::env::set_var("TEST_XAI_API_KEY", "xai-real-secret-token-99999");
        }

        let mut config = GmConfig {
            storage_dir: dir.clone(),
            ..Default::default()
        };
        config.providers.profiles[0].account_id = Some("private-account-email@example.com".into());
        config.providers.profiles[0].base_url = "https://user:pass@custom.example/v1?key=1".into();
        config.providers.profiles[0].last_error = Some("raw error with sk-proj-secret123".into());
        config
            .safety
            .blocklisted_apps
            .push("super-confidential-token-12345".into());
        let bundle = export_support_bundle(&config, &export_path).unwrap();

        assert!(export_path.exists());
        assert!(bundle.redaction_verified);
        assert!(!bundle.sha256_fingerprint.is_empty());

        let file_contents = std::fs::read_to_string(&export_path).unwrap();
        assert!(!file_contents.contains("super-confidential-token-12345"));
        assert!(!file_contents.contains("xai-real-secret-token-99999"));
        assert!(!file_contents.contains("private-account-email@example.com"));
        assert!(!file_contents.contains("user:pass"));
        assert!(!file_contents.contains("sk-proj-secret123"));
        assert!(!file_contents.contains(&dir.display().to_string()));
        if let Ok(home) = std::env::var("HOME")
            && home.len() >= 4
        {
            assert!(!file_contents.contains(&home));
        }

        unsafe {
            std::env::remove_var("TEST_DUMMY_SECRET_KEY");
            std::env::remove_var("TEST_XAI_API_KEY");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
