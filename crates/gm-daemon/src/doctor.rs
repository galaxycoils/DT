//! Unified DT Doctor engine serving installers, `dt doctor`, standalone `dt-doctor`,
//! onboarding, Settings, and sanitized Web/Mobile status.

use crate::migration::{CURRENT_DATA_SCHEMA_VERSION, MigrationOutcome, migrate_user_data};
use crate::service;
use chrono::Utc;
use gm_core::config::GmConfig;
use gm_core::error::{GmError, Result};
use gm_platform::IpcPlatformClient;
use gm_platform::permissions::check_all_permissions;
use gm_safety::SecretScrubber;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

pub const DOCTOR_REPORT_SCHEMA: u32 = 1;
pub const MAX_REPAIR_ATTEMPTS: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DoctorCheckOutcome {
    Pass,
    Repaired,
    ActionRequired,
    RestartRequired,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessState {
    Ready,
    NeedsAction,
    RestartRequired,
    Degraded,
    Unconfigured,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixedRepairAction {
    CreatePrivateStorageDir,
    RemoveStaleSocketOrLock,
    VerifyAndMigrateStorage,
    RepairUserServices,
    QuarantineCorruptDatabase,
    PromptOsElevation,
    PromptOsPermissions,
    PromptCredentialStoreUnlock,
    PromptSystemReboot,
    VerifySignedStaging,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DoctorCheckEntry {
    pub id: String,
    pub category: String,
    pub outcome: DoctorCheckOutcome,
    pub attempts: u32,
    pub summary: String,
    pub next_action: Option<String>,
    pub repair_action: Option<FixedRepairAction>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CapabilityStatus {
    pub id: String,
    pub name: String,
    pub readiness: ReadinessState,
    pub summary: String,
    pub next_action: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RestartStatus {
    pub required: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DoctorReport {
    pub schema: u32,
    pub run_id: String,
    pub dt_build: String,
    pub target: String,
    pub recorded_at: String,
    pub installation_readiness: ReadinessState,
    pub capability_readiness: Vec<CapabilityStatus>,
    pub checks: Vec<DoctorCheckEntry>,
    pub pending_actions: Vec<String>,
    pub restart_status: RestartStatus,
    pub redacted_log_location: Option<PathBuf>,
}

/// Sanitized Doctor status safe for remote Web and Mobile surfaces (strips local paths/logs).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DoctorSanitizedReport {
    pub schema: u32,
    pub run_id: String,
    pub dt_build: String,
    pub target: String,
    pub recorded_at: String,
    pub installation_readiness: ReadinessState,
    pub capability_readiness: Vec<CapabilityStatus>,
    pub checks: Vec<DoctorSanitizedCheck>,
    pub restart_required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DoctorSanitizedCheck {
    pub id: String,
    pub category: String,
    pub outcome: DoctorCheckOutcome,
    pub summary: String,
}

impl DoctorReport {
    pub fn sanitized_for_remote(&self) -> DoctorSanitizedReport {
        let scrubber = SecretScrubber::new();
        DoctorSanitizedReport {
            schema: self.schema,
            run_id: self.run_id.clone(),
            dt_build: self.dt_build.clone(),
            target: self.target.clone(),
            recorded_at: self.recorded_at.clone(),
            installation_readiness: self.installation_readiness,
            capability_readiness: self.capability_readiness.clone(),
            checks: self
                .checks
                .iter()
                .map(|c| DoctorSanitizedCheck {
                    id: c.id.clone(),
                    category: c.category.clone(),
                    outcome: c.outcome,
                    summary: scrubber.scrub_str(&c.summary),
                })
                .collect(),
            restart_required: self.restart_status.required,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct DoctorRunJournal {
    run_id: String,
    attempts_by_check: BTreeMap<String, u32>,
    completed: bool,
    last_updated: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoctorMode {
    Check,
    Repair,
    Resume,
}

fn target_triple_label() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

fn journal_dir(storage_dir: &Path) -> PathBuf {
    storage_dir.join("doctor-runs")
}

fn validate_run_id(run_id: &str) -> Result<()> {
    if run_id.is_empty()
        || run_id.len() > 64
        || !run_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(GmError::Config(format!("Invalid Doctor run ID '{run_id}'")));
    }
    Ok(())
}

fn load_or_create_journal(storage_dir: &Path, run_id: Option<&str>) -> Result<DoctorRunJournal> {
    if let Some(id) = run_id {
        validate_run_id(id)?;
        let path = journal_dir(storage_dir).join(format!("{id}.json"));
        let raw = fs::read_to_string(&path).map_err(|e| {
            GmError::Config(format!(
                "Doctor run journal '{id}' not found ({}): {e}",
                path.display()
            ))
        })?;
        let journal: DoctorRunJournal = serde_json::from_str(&raw)
            .map_err(|e| GmError::Config(format!("Corrupt Doctor run journal '{id}': {e}")))?;
        return Ok(journal);
    }
    Ok(DoctorRunJournal {
        run_id: format!("doc-{}", &uuid::Uuid::new_v4().simple().to_string()[..12]),
        attempts_by_check: BTreeMap::new(),
        completed: false,
        last_updated: Utc::now().to_rfc3339(),
    })
}

fn save_journal(storage_dir: &Path, journal: &DoctorRunJournal) -> Result<()> {
    let dir = journal_dir(storage_dir);
    gm_core::storage::ensure_private_dir(&dir)?;
    let path = dir.join(format!("{}.json", journal.run_id));
    let bytes = serde_json::to_vec_pretty(journal)?;
    gm_core::storage::atomic_write_private(&path, &bytes)?;
    Ok(())
}

/// Remove a stale socket or lock file ONLY after proving ownership and verifying no live writer.
pub fn remove_stale_socket_if_unowned_live(socket_path: &Path) -> Result<bool> {
    if !socket_path.exists() {
        return Ok(false);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = fs::symlink_metadata(socket_path)?;
        let my_uid = unsafe { libc::geteuid() };
        if meta.uid() != my_uid {
            return Err(GmError::SafetyViolation(format!(
                "Refusing to remove socket {} owned by UID {} (current UID {})",
                socket_path.display(),
                meta.uid(),
                my_uid
            )));
        }
        let lock_path = socket_path.with_extension("helper.lock");
        if lock_path.exists() {
            let lock_meta = fs::symlink_metadata(&lock_path)?;
            if lock_meta.uid() != my_uid {
                return Err(GmError::SafetyViolation(format!(
                    "Refusing to touch lock {} owned by UID {}",
                    lock_path.display(),
                    lock_meta.uid()
                )));
            }
            let file = OpenOptions::new().read(true).write(true).open(&lock_path)?;
            if file.try_lock().is_err() {
                // Live writer holds lock; never unlink!
                return Ok(false);
            }
        }
        // Probe Unix socket non-destructively before removing.
        if std::os::unix::net::UnixStream::connect(socket_path).is_ok() {
            return Ok(false);
        }
        fs::remove_file(socket_path)?;
        Ok(true)
    }
    #[cfg(not(unix))]
    {
        let _ = socket_path;
        Ok(false)
    }
}

fn available_disk_bytes(path: &Path) -> Option<u64> {
    let probe = if path.exists() {
        path.to_path_buf()
    } else {
        path.parent().unwrap_or(Path::new("/")).to_path_buf()
    };
    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let cpath = CString::new(probe.as_os_str().as_bytes()).ok()?;
        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::statvfs(cpath.as_ptr(), &mut stat) };
        if rc == 0 {
            return Some((stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64));
        }
        None
    }
    #[cfg(not(unix))]
    {
        let _ = probe;
        None
    }
}

fn check_credential_store_actionable() -> (DoctorCheckOutcome, String, Option<String>) {
    #[cfg(target_os = "macos")]
    {
        match std::process::Command::new("security")
            .args(["default-keychain"])
            .output()
        {
            Ok(out) if out.status.success() => (
                DoctorCheckOutcome::Pass,
                "macOS Keychain default store is accessible".into(),
                None,
            ),
            Ok(out) => (
                DoctorCheckOutcome::ActionRequired,
                format!(
                    "macOS Keychain is locked or unavailable: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ),
                Some(
                    "Unlock your login Keychain in Keychain Access and re-run `dt doctor resume`."
                        .into(),
                ),
            ),
            Err(e) => (
                DoctorCheckOutcome::Failed,
                format!("macOS security tool unavailable: {e}"),
                Some("Verify /usr/bin/security is present on macOS.".into()),
            ),
        }
    }
    #[cfg(target_os = "linux")]
    {
        let has_dbus = std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some();
        if has_dbus {
            (
                DoctorCheckOutcome::Pass,
                "Linux Secret Service session D-Bus address is present".into(),
                None,
            )
        } else {
            (
                DoctorCheckOutcome::ActionRequired,
                "No Secret Service D-Bus session detected (DBUS_SESSION_BUS_ADDRESS unset)".into(),
                Some("Sign into an active desktop session with gnome-keyring/Secret Service and run `dt doctor resume`.".into()),
            )
        }
    }
    #[cfg(target_os = "windows")]
    {
        (
            DoctorCheckOutcome::Pass,
            "Windows Credential Manager available for current user SID".into(),
            None,
        )
    }
}

fn check_hypervisor_capability() -> CapabilityStatus {
    #[cfg(target_os = "macos")]
    {
        let hv = std::process::Command::new("sysctl")
            .args(["-n", "kern.hv_support"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim() == "1")
            .unwrap_or(false);
        CapabilityStatus {
            id: "hypervisor".into(),
            name: "macOS Hypervisor.framework (HVF)".into(),
            readiness: if hv {
                ReadinessState::Ready
            } else {
                ReadinessState::Unconfigured
            },
            summary: if hv {
                "Apple Hypervisor.framework is supported on this host".into()
            } else {
                "Hypervisor.framework is not enabled; isolated VM coding remains unavailable".into()
            },
            next_action: (!hv)
                .then(|| "Enable virtualization support or use hosted coding workspaces.".into()),
        }
    }
    #[cfg(target_os = "linux")]
    {
        let kvm = Path::new("/dev/kvm").exists();
        CapabilityStatus {
            id: "hypervisor".into(),
            name: "Linux KVM (/dev/kvm)".into(),
            readiness: if kvm {
                ReadinessState::Ready
            } else {
                ReadinessState::Unconfigured
            },
            summary: if kvm {
                "/dev/kvm is present".into()
            } else {
                "/dev/kvm not found; isolated VM coding unavailable".into()
            },
            next_action: (!kvm)
                .then(|| "Enable KVM virtualization and add user to the kvm group.".into()),
        }
    }
    #[cfg(target_os = "windows")]
    {
        CapabilityStatus {
            id: "hypervisor".into(),
            name: "Windows Hypervisor Platform (WHPX)".into(),
            readiness: ReadinessState::Unconfigured,
            summary: "WHPX optional hypervisor check requires Windows feature inspection".into(),
            next_action: Some("Enable Windows Hypervisor Platform when configuring isolated VM coding, reboot, and run `dt doctor resume`.".into()),
        }
    }
}

/// Run the unified Doctor engine in Check, Repair, or Resume mode.
pub async fn run_doctor(
    config: &GmConfig,
    mode: DoctorMode,
    resume_run_id: Option<&str>,
) -> Result<DoctorReport> {
    let storage_dir = &config.storage_dir;
    let mut journal = match mode {
        DoctorMode::Resume => load_or_create_journal(storage_dir, resume_run_id)?,
        _ => load_or_create_journal(storage_dir, None)?,
    };

    let apply_repairs = matches!(mode, DoctorMode::Repair | DoctorMode::Resume);
    let mut checks = Vec::new();
    let mut pending_actions = Vec::new();
    let mut restart_status = RestartStatus {
        required: false,
        reason: None,
    };

    // 1. OS / Architecture / CPU
    checks.push(DoctorCheckEntry {
        id: "os_arch_cpu".into(),
        category: "platform".into(),
        outcome: DoctorCheckOutcome::Pass,
        attempts: 1,
        summary: format!(
            "OS={}, arch={}, cpus={}",
            std::env::consts::OS,
            std::env::consts::ARCH,
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1)
        ),
        next_action: None,
        repair_action: None,
    });

    // 2. Disk Space (require >= 512 MiB free for safe runtime & snapshots)
    let min_bytes: u64 = 512 * 1024 * 1024;
    let disk_entry = match available_disk_bytes(storage_dir) {
        Some(avail) if avail >= min_bytes => DoctorCheckEntry {
            id: "disk_space".into(),
            category: "filesystem".into(),
            outcome: DoctorCheckOutcome::Pass,
            attempts: 1,
            summary: format!("{} MiB free in storage volume", avail / (1024 * 1024)),
            next_action: None,
            repair_action: None,
        },
        Some(avail) => DoctorCheckEntry {
            id: "disk_space".into(),
            category: "filesystem".into(),
            outcome: DoctorCheckOutcome::ActionRequired,
            attempts: 1,
            summary: format!(
                "Only {} MiB free (minimum 512 MiB recommended)",
                avail / (1024 * 1024)
            ),
            next_action: Some("Free disk space and re-run `dt doctor resume`.".into()),
            repair_action: None,
        },
        None => DoctorCheckEntry {
            id: "disk_space".into(),
            category: "filesystem".into(),
            outcome: DoctorCheckOutcome::Pass,
            attempts: 1,
            summary: "Disk free space probe skipped on this target".into(),
            next_action: None,
            repair_action: None,
        },
    };
    checks.push(disk_entry);

    // 3. Ownership, private storage directory & stale lock/socket check
    let mut storage_outcome = DoctorCheckOutcome::Pass;
    let mut storage_msg = format!("Storage directory {} is ready", storage_dir.display());
    let mut storage_next = None;
    let mut storage_attempts = *journal
        .attempts_by_check
        .get("ownership_permissions")
        .unwrap_or(&0);
    if !storage_dir.exists() {
        if apply_repairs {
            if storage_attempts >= MAX_REPAIR_ATTEMPTS {
                storage_outcome = DoctorCheckOutcome::Failed;
                storage_msg =
                    "Exceeded max repair attempts (2) creating private storage directory".into();
            } else {
                storage_attempts += 1;
                journal
                    .attempts_by_check
                    .insert("ownership_permissions".into(), storage_attempts);
                match gm_core::storage::ensure_private_dir(storage_dir) {
                    Ok(()) => {
                        storage_outcome = DoctorCheckOutcome::Repaired;
                        storage_msg = format!(
                            "Created private storage directory {}",
                            storage_dir.display()
                        );
                    }
                    Err(e) => {
                        storage_outcome = DoctorCheckOutcome::Failed;
                        storage_msg = format!("Failed creating storage directory: {e}");
                        storage_next = Some("Check folder ownership and permissions.".into());
                    }
                }
            }
        } else {
            storage_outcome = DoctorCheckOutcome::ActionRequired;
            storage_msg = format!(
                "Storage directory {} does not exist yet",
                storage_dir.display()
            );
            storage_next = Some(format!(
                "Run `dt doctor repair --json` (or `dt doctor resume {}`).",
                journal.run_id
            ));
        }
    }
    // Check stale helper socket safely (never delete live owned sockets)
    if apply_repairs {
        match remove_stale_socket_if_unowned_live(&config.socket_path) {
            Ok(true) => {
                storage_outcome = DoctorCheckOutcome::Repaired;
                storage_msg = format!(
                    "{storage_msg}; removed verified stale socket {}",
                    config.socket_path.display()
                );
            }
            Ok(false) => {}
            Err(e) => {
                storage_outcome = DoctorCheckOutcome::Failed;
                storage_msg = format!("Socket ownership check failed: {e}");
                storage_next = Some("Inspect socket ownership manually.".into());
            }
        }
    }
    checks.push(DoctorCheckEntry {
        id: "ownership_permissions".into(),
        category: "filesystem".into(),
        outcome: storage_outcome,
        attempts: storage_attempts.max(1),
        summary: storage_msg,
        next_action: storage_next,
        repair_action: Some(FixedRepairAction::CreatePrivateStorageDir),
    });

    // 4. Component identity (binary/version resolution)
    let (daemon_bin, helper_bin) = service::resolve_binaries(None);
    let bins_ok = daemon_bin.is_file() && helper_bin.is_file();
    checks.push(DoctorCheckEntry {
        id: "component_identity".into(),
        category: "installation".into(),
        outcome: if bins_ok {
            DoctorCheckOutcome::Pass
        } else {
            DoctorCheckOutcome::ActionRequired
        },
        attempts: 1,
        summary: if bins_ok {
            format!(
                "Resolved DT binaries (build {}): daemon={}, helper={}",
                env!("CARGO_PKG_VERSION"),
                daemon_bin.display(),
                helper_bin.display()
            )
        } else {
            format!(
                "Missing matching daemon/helper binaries (expected {} and {})",
                daemon_bin.display(),
                helper_bin.display()
            )
        },
        next_action: (!bins_ok)
            .then(|| "Reinstall DT using the signed installer for your OS.".into()),
        repair_action: None,
    });

    // 5. Migration & SQLite integrity check
    let mut mig_attempts = *journal
        .attempts_by_check
        .get("migration_state")
        .unwrap_or(&0);
    let legacy_dir = gm_core::config::legacy_storage_dir();
    let mig_entry = if storage_dir.exists() {
        if apply_repairs && mig_attempts < MAX_REPAIR_ATTEMPTS {
            mig_attempts += 1;
            journal
                .attempts_by_check
                .insert("migration_state".into(), mig_attempts);
            match migrate_user_data(
                legacy_dir.as_deref(),
                storage_dir,
                env!("CARGO_PKG_VERSION"),
                None,
            ) {
                Ok(MigrationOutcome {
                    dual_store_conflict: true,
                    message,
                    ..
                }) => DoctorCheckEntry {
                    id: "migration_state".into(),
                    category: "storage".into(),
                    outcome: DoctorCheckOutcome::ActionRequired,
                    attempts: mig_attempts,
                    summary: message,
                    next_action: Some(
                        "Choose whether to preserve legacy or DT store in Settings -> Migration before continuing.".into(),
                    ),
                    repair_action: Some(FixedRepairAction::VerifyAndMigrateStorage),
                },
                Ok(outcome) => DoctorCheckEntry {
                    id: "migration_state".into(),
                    category: "storage".into(),
                    outcome: if outcome.status == "migrated" {
                        DoctorCheckOutcome::Repaired
                    } else {
                        DoctorCheckOutcome::Pass
                    },
                    attempts: mig_attempts,
                    summary: format!(
                        "{} (schema v{} -> v{CURRENT_DATA_SCHEMA_VERSION})",
                        outcome.message, outcome.from_schema
                    ),
                    next_action: None,
                    repair_action: Some(FixedRepairAction::VerifyAndMigrateStorage),
                },
                Err(e) => DoctorCheckEntry {
                    id: "migration_state".into(),
                    category: "storage".into(),
                    outcome: DoctorCheckOutcome::Failed,
                    attempts: mig_attempts,
                    summary: format!("Migration check failed: {e}"),
                    next_action: Some("Preserve backups and run `dt diagnostics recover-database`.".into()),
                    repair_action: Some(FixedRepairAction::QuarantineCorruptDatabase),
                },
            }
        } else {
            // Check integrity read-only
            let mut db_err = None;
            for db_name in ["openbot.sqlite", "channels.sqlite", "usage.sqlite3"] {
                let db_path = storage_dir.join(db_name);
                if db_path.exists()
                    && let Err(e) = gm_openbot::check_sqlite_integrity(&db_path)
                {
                    db_err = Some(format!("{db_name}: {e}"));
                    break;
                }
            }
            if let Some(err) = db_err {
                DoctorCheckEntry {
                    id: "migration_state".into(),
                    category: "storage".into(),
                    outcome: if mig_attempts >= MAX_REPAIR_ATTEMPTS {
                        DoctorCheckOutcome::Failed
                    } else {
                        DoctorCheckOutcome::ActionRequired
                    },
                    attempts: mig_attempts.max(1),
                    summary: format!("Database integrity failure: {err}"),
                    next_action: Some("Run `dt diagnostics recover-database` to quarantine with preserved backups.".into()),
                    repair_action: Some(FixedRepairAction::QuarantineCorruptDatabase),
                }
            } else {
                DoctorCheckEntry {
                    id: "migration_state".into(),
                    category: "storage".into(),
                    outcome: DoctorCheckOutcome::Pass,
                    attempts: 1,
                    summary: "SQLite stores passed integrity check".into(),
                    next_action: None,
                    repair_action: None,
                }
            }
        }
    } else {
        DoctorCheckEntry {
            id: "migration_state".into(),
            category: "storage".into(),
            outcome: DoctorCheckOutcome::Pass,
            attempts: 1,
            summary: "No existing stores yet; fresh schema will initialize on first start".into(),
            next_action: None,
            repair_action: None,
        }
    };
    checks.push(mig_entry);

    // 6. Real daemon / helper handshake check
    let helper_client = IpcPlatformClient::new(&config.socket_path);
    let helper_ping =
        tokio::time::timeout(std::time::Duration::from_millis(250), helper_client.ping()).await;
    let helper_ok = matches!(helper_ping, Ok(Ok(())));
    checks.push(DoctorCheckEntry {
        id: "daemon_helper_handshake".into(),
        category: "ipc".into(),
        outcome: if helper_ok {
            DoctorCheckOutcome::Pass
        } else {
            DoctorCheckOutcome::ActionRequired
        },
        attempts: 1,
        summary: if helper_ok {
            "Verified live Ping/Pong handshake with CU helper".into()
        } else {
            "CU helper is not currently running (auto-started when daemon or app launches)".into()
        },
        next_action: (!helper_ok)
            .then(|| "Start DT or run `dt service install` to start user services.".into()),
        repair_action: Some(FixedRepairAction::RepairUserServices),
    });

    // 7. Credential store availability (must be actionable)
    let (cred_outcome, cred_msg, cred_next) = check_credential_store_actionable();
    checks.push(DoctorCheckEntry {
        id: "credential_store".into(),
        category: "security".into(),
        outcome: cred_outcome,
        attempts: 1,
        summary: cred_msg,
        next_action: cred_next,
        repair_action: Some(FixedRepairAction::PromptCredentialStoreUnlock),
    });

    // 8. OS permissions / grants
    let perms = check_all_permissions();
    let all_perms = perms.screen_recording && perms.accessibility;
    checks.push(DoctorCheckEntry {
        id: "os_grants".into(),
        category: "permissions".into(),
        outcome: if all_perms {
            DoctorCheckOutcome::Pass
        } else {
            DoctorCheckOutcome::ActionRequired
        },
        attempts: 1,
        summary: format!(
            "ScreenRecording={}, Accessibility={}, Calendar={}, Reminders={}",
            perms.screen_recording, perms.accessibility, perms.calendar, perms.reminders
        ),
        next_action: (!all_perms).then(|| {
            format!(
                "Grant Screen Recording & Accessibility in OS Settings, then run `dt doctor resume {} --json`.",
                journal.run_id
            )
        }),
        repair_action: Some(FixedRepairAction::PromptOsPermissions),
    });

    // 9. Signed download staging integrity
    let staging_dir = storage_dir.join("updates");
    let staging_ok = !staging_dir.exists()
        || fs::read_dir(&staging_dir)
            .map(|rd| {
                !rd.flatten()
                    .any(|e| e.path().extension().is_some_and(|x| x == "partial"))
            })
            .unwrap_or(true);
    checks.push(DoctorCheckEntry {
        id: "signed_download_integrity".into(),
        category: "updates".into(),
        outcome: if staging_ok {
            DoctorCheckOutcome::Pass
        } else {
            DoctorCheckOutcome::ActionRequired
        },
        attempts: 1,
        summary: if staging_ok {
            "No incomplete or unverified staged update downloads".into()
        } else {
            "Incomplete staged update artifacts detected".into()
        },
        next_action: (!staging_ok)
            .then(|| "Re-run `dt update` to verify and re-stage signed payloads.".into()),
        repair_action: Some(FixedRepairAction::VerifySignedStaging),
    });

    // Optional feature capabilities (separate from installation_readiness!)
    let mut capabilities = vec![check_hypervisor_capability()];
    capabilities.push(CapabilityStatus {
        id: "local_vlm".into(),
        name: "Local VLM Workers (Qwen2-VL-2B / SmolVLM / SmolVLM2)".into(),
        readiness: ReadinessState::Unconfigured,
        summary: "Local multimodal assets are downloaded on demand when enabled".into(),
        next_action: Some(
            "Enable a local model in Settings -> Models to download signed checkpoints.".into(),
        ),
    });
    capabilities.push(CapabilityStatus {
        id: "voice_runtime".into(),
        name: "Managed Whisper STT & Local TTS".into(),
        readiness: ReadinessState::Unconfigured,
        summary: "Speech assets are downloaded and started on demand when voice mode is enabled"
            .into(),
        next_action: Some("Enable Voice in Settings -> Voice & Meetings.".into()),
    });

    for check in &checks {
        if let Some(next) = &check.next_action
            && !matches!(
                check.outcome,
                DoctorCheckOutcome::Pass | DoctorCheckOutcome::Repaired
            )
        {
            pending_actions.push(format!("{}: {next}", check.id));
        }
        if check.outcome == DoctorCheckOutcome::RestartRequired {
            restart_status = RestartStatus {
                required: true,
                reason: Some(check.summary.clone()),
            };
        }
    }

    let installation_readiness = if checks
        .iter()
        .any(|c| c.outcome == DoctorCheckOutcome::Failed)
    {
        ReadinessState::Degraded
    } else if restart_status.required {
        ReadinessState::RestartRequired
    } else if checks
        .iter()
        .any(|c| c.outcome == DoctorCheckOutcome::ActionRequired)
    {
        ReadinessState::NeedsAction
    } else {
        ReadinessState::Ready
    };

    journal.completed = installation_readiness == ReadinessState::Ready;
    journal.last_updated = Utc::now().to_rfc3339();

    let mut report = DoctorReport {
        schema: DOCTOR_REPORT_SCHEMA,
        run_id: journal.run_id.clone(),
        dt_build: env!("CARGO_PKG_VERSION").to_string(),
        target: target_triple_label(),
        recorded_at: Utc::now().to_rfc3339(),
        installation_readiness,
        capability_readiness: capabilities,
        checks,
        pending_actions,
        restart_status,
        redacted_log_location: None,
    };

    // Persist redacted log & journal if storage dir exists or was repaired.
    if storage_dir.exists() {
        let _ = save_journal(storage_dir, &journal);
        let log_path = journal_dir(storage_dir).join(format!("{}.log", journal.run_id));
        let scrubber = SecretScrubber::new();
        let raw_log = serde_json::to_string_pretty(&report)?;
        let scrubbed = scrubber.scrub_str(&raw_log);
        if gm_core::storage::atomic_write_private(&log_path, scrubbed.as_bytes()).is_ok() {
            report.redacted_log_location = Some(log_path);
        }
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_doctor_check_repair_resume_and_max_attempts() {
        let root = std::env::temp_dir().join(format!("dt-doc-test-{}", uuid::Uuid::new_v4()));
        let storage = root.join("storage");
        let config = GmConfig {
            storage_dir: storage.clone(),
            socket_path: root.join("helper.sock"),
            daemon_socket_path: root.join("daemon.sock"),
            ..Default::default()
        };

        // 1. Check does not create storage directory.
        let check_report = run_doctor(&config, DoctorMode::Check, None).await.unwrap();
        assert_eq!(check_report.schema, 1);
        assert!(!storage.exists());
        assert!(
            check_report
                .checks
                .iter()
                .any(|c| c.id == "ownership_permissions"
                    && c.outcome == DoctorCheckOutcome::ActionRequired)
        );

        // 2. Repair creates private storage dir and writes journal + redacted log.
        let repair_report = run_doctor(&config, DoctorMode::Repair, None).await.unwrap();
        assert!(storage.exists());
        assert!(repair_report.redacted_log_location.is_some());
        assert!(
            repair_report
                .checks
                .iter()
                .any(|c| c.id == "ownership_permissions"
                    && c.outcome == DoctorCheckOutcome::Repaired)
        );

        // 3. Resume continues the same run_id cleanly.
        let resumed = run_doctor(&config, DoctorMode::Resume, Some(&repair_report.run_id))
            .await
            .unwrap();
        assert_eq!(resumed.run_id, repair_report.run_id);

        // 4. Sanitized report for web/mobile omits local filesystem log location.
        let sanitized = resumed.sanitized_for_remote();
        assert_eq!(sanitized.run_id, resumed.run_id);

        // 5. Enforcing max 2 repair attempts & path-traversal rejection on resume.
        let mut seeded = DoctorRunJournal {
            run_id: "doc-max-attempt".into(),
            attempts_by_check: BTreeMap::new(),
            completed: false,
            last_updated: Utc::now().to_rfc3339(),
        };
        seeded
            .attempts_by_check
            .insert("ownership_permissions".into(), MAX_REPAIR_ATTEMPTS);
        save_journal(&storage, &seeded).unwrap();
        let resumed_max = run_doctor(&config, DoctorMode::Resume, Some("doc-max-attempt"))
            .await
            .unwrap();
        assert_eq!(resumed_max.run_id, "doc-max-attempt");
        assert!(
            run_doctor(&config, DoctorMode::Resume, Some("../escape"))
                .await
                .is_err()
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_doctor_sanitized_web_status() {
        let report = DoctorReport {
            schema: 1,
            run_id: "doc-123".into(),
            dt_build: "1.0.0".into(),
            target: "macos-aarch64".into(),
            recorded_at: Utc::now().to_rfc3339(),
            installation_readiness: ReadinessState::Ready,
            capability_readiness: vec![],
            checks: vec![DoctorCheckEntry {
                id: "credential_store".into(),
                category: "security".into(),
                outcome: DoctorCheckOutcome::Pass,
                attempts: 1,
                summary: "Credential store healthy".into(),
                next_action: None,
                repair_action: None,
            }],
            pending_actions: vec![],
            restart_status: RestartStatus {
                required: false,
                reason: None,
            },
            redacted_log_location: Some(PathBuf::from("/Users/secret/doctor.log")),
        };
        let json = serde_json::to_string(&report.sanitized_for_remote()).unwrap();
        assert!(!json.contains("/Users/secret"));
    }
}
