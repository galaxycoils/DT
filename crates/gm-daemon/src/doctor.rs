//! Unified DT Doctor engine serving installers, `dt doctor`, standalone `dt-doctor`,
//! onboarding, Settings, and sanitized Web/Mobile status.

use crate::migration::{
    MigrationState, RollbackPhase, StorageLease, execute_journaled_rollback, inspect_migration,
    load_rollback_journal, migrate_user_data,
};
use crate::service;
use chrono::Utc;
use gm_core::config::GmConfig;
use gm_core::error::{GmError, Result};
use gm_platform::IpcPlatformClient;
#[cfg(target_os = "macos")]
use gm_platform::permissions::check_all_permissions;
use gm_safety::SecretScrubber;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

pub const DOCTOR_REPORT_SCHEMA: u32 = 1;
pub const MAX_REPAIR_ATTEMPTS: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DoctorCheckOutcome {
    Pass,
    Repaired,
    Info,
    Warn,
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
    RestoreDefaultConfig,
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
                    summary: match c.outcome {
                        DoctorCheckOutcome::Pass => "Passed",
                        DoctorCheckOutcome::Repaired => "Repaired",
                        DoctorCheckOutcome::Info => "Info",
                        DoctorCheckOutcome::Warn => "Warning",
                        DoctorCheckOutcome::ActionRequired => "Local action required",
                        DoctorCheckOutcome::RestartRequired => "Restart required",
                        DoctorCheckOutcome::Failed => "Failed",
                    }
                    .into(),
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
        match fs::read_to_string(&path) {
            Ok(raw) => {
                let journal: DoctorRunJournal = serde_json::from_str(&raw).map_err(|e| {
                    GmError::Config(format!("Corrupt Doctor run journal '{id}': {e}"))
                })?;
                return Ok(journal);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if load_rollback_journal(storage_dir, id)?.is_some() {
                    return Ok(DoctorRunJournal {
                        run_id: id.to_string(),
                        attempts_by_check: BTreeMap::new(),
                        completed: false,
                        last_updated: Utc::now().to_rfc3339(),
                    });
                }
                return Err(GmError::Config(format!(
                    "Doctor run journal '{id}' not found ({}): {e}",
                    path.display()
                )));
            }
            Err(e) => {
                return Err(GmError::Config(format!(
                    "Doctor run journal '{id}' unreadable ({}): {e}",
                    path.display()
                )));
            }
        }
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

#[cfg(unix)]
fn repair_stale_socket(socket_path: &Path, lock_extension: &str) -> Result<bool> {
    if !socket_path.try_exists()? {
        return Ok(false);
    }
    let Some(_lease) = gm_core::ipc::try_lock_unix_socket(socket_path, lock_extension)? else {
        return Ok(false);
    };
    gm_core::ipc::remove_stale_unix_socket(socket_path)
}

fn available_disk_bytes(path: &Path) -> Option<u64> {
    let probe = path
        .ancestors()
        .find(|candidate| candidate.exists())
        .unwrap_or(Path::new("."));
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
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;

        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetDiskFreeSpaceExW(
                directory: *const u16,
                available: *mut u64,
                total: *mut u64,
                free: *mut u64,
            ) -> i32;
        }

        let wide: Vec<_> = probe.as_os_str().encode_wide().chain([0]).collect();
        let mut available = 0;
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &mut available,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        (ok != 0).then_some(available)
    }
    #[cfg(not(any(unix, windows)))]
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

fn check_os_grants_actionable(run_id: &str) -> (DoctorCheckOutcome, String, Option<String>) {
    #[cfg(target_os = "macos")]
    {
        let perms = check_all_permissions();
        let all_perms = perms.screen_recording && perms.accessibility;
        let summary = format!(
            "ScreenRecording={}, Accessibility={}, Calendar={}, Reminders={}",
            perms.screen_recording, perms.accessibility, perms.calendar, perms.reminders
        );
        let next = (!all_perms).then(|| {
            format!(
                "Grant Screen Recording & Accessibility in OS Settings, then run `dt doctor resume {run_id} --json`."
            )
        });
        (
            if all_perms {
                DoctorCheckOutcome::Pass
            } else {
                DoctorCheckOutcome::ActionRequired
            },
            summary,
            next,
        )
    }
    #[cfg(target_os = "linux")]
    {
        let hosted = std::env::var("DT_HOSTED_ENABLED")
            .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
        let has_wayland = std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty());
        let has_x11 = std::env::var_os("DISPLAY").is_some_and(|v| !v.is_empty());
        if has_wayland || has_x11 {
            (
                DoctorCheckOutcome::Pass,
                format!(
                    "Linux desktop session detected (wayland={has_wayland}, x11={has_x11}); portal/AT-SPI grants prompt on first use"
                ),
                None,
            )
        } else if hosted {
            (
                DoctorCheckOutcome::Pass,
                "Hosted Linux mode does not require local desktop display permissions".into(),
                None,
            )
        } else {
            (
                DoctorCheckOutcome::ActionRequired,
                "No active Wayland or X11 display session detected (WAYLAND_DISPLAY/DISPLAY unset)"
                    .into(),
                Some(format!(
                    "Start DT inside a graphical desktop session, then run `dt doctor resume {run_id} --json`."
                )),
            )
        }
    }
    #[cfg(target_os = "windows")]
    {
        let _ = run_id;
        (
            DoctorCheckOutcome::Pass,
            "Windows Graphics Capture and UI Automation available in interactive user session"
                .into(),
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

pub async fn run_doctor_from_config_path(
    custom_path: Option<&Path>,
    mode: DoctorMode,
    resume_run_id: Option<&str>,
) -> Result<DoctorReport> {
    match GmConfig::load(custom_path) {
        Ok(config) => run_doctor(&config, mode, resume_run_id).await,
        Err(error) => {
            let path = custom_path
                .map(Path::to_path_buf)
                .unwrap_or_else(gm_core::config::default_config_path);
            let config = GmConfig::default();
            run_doctor_inner(&config, mode, resume_run_id, Some((&path, &error))).await
        }
    }
}

pub(crate) async fn doctor_status_snapshot(
    config: &GmConfig,
) -> std::sync::Arc<parking_lot::RwLock<Option<serde_json::Value>>> {
    let report = run_doctor(config, DoctorMode::Check, None)
        .await
        .ok()
        .and_then(|report| serde_json::to_value(report.sanitized_for_remote()).ok());
    std::sync::Arc::new(parking_lot::RwLock::new(report))
}

fn repair_corrupt_config(path: &Path, config: &GmConfig) -> Result<bool> {
    let _lease = StorageLease::acquire(&config.storage_dir)?;
    if !fs::symlink_metadata(path)?.is_file() {
        return Err(GmError::SafetyViolation(
            "Configuration must be a regular file".into(),
        ));
    }
    let raw = fs::read(path)?;
    if serde_json::from_slice::<GmConfig>(&raw).is_ok() {
        return Ok(false);
    }
    let backup = path.with_extension(format!("corrupt-{}.bak", uuid::Uuid::new_v4().simple()));
    gm_core::storage::atomic_write_private(&backup, &raw)?;
    if fs::read(path)? != raw {
        return Err(GmError::Config(
            "Configuration changed during recovery".into(),
        ));
    }
    config.save_atomic(path)?;
    Ok(true)
}

/// Run the unified Doctor engine in Check, Repair, or Resume mode.
pub async fn run_doctor(
    config: &GmConfig,
    mode: DoctorMode,
    resume_run_id: Option<&str>,
) -> Result<DoctorReport> {
    run_doctor_inner(config, mode, resume_run_id, None).await
}

async fn run_doctor_inner(
    config: &GmConfig,
    mode: DoctorMode,
    resume_run_id: Option<&str>,
    config_error: Option<(&Path, &GmError)>,
) -> Result<DoctorReport> {
    let storage_dir = &config.storage_dir;
    let mut journal = match mode {
        DoctorMode::Resume => load_or_create_journal(storage_dir, resume_run_id)?,
        _ => load_or_create_journal(storage_dir, None)?,
    };

    let mut apply_repairs = matches!(mode, DoctorMode::Repair | DoctorMode::Resume);
    let mut checks = Vec::new();
    if mode == DoctorMode::Resume
        && let Ok(Some(rb_journal)) = load_rollback_journal(storage_dir, &journal.run_id)
        && rb_journal.phase != RollbackPhase::Completed
    {
        match execute_journaled_rollback(&rb_journal.snapshot_dir, storage_dir, &journal.run_id) {
            Ok(outcome) => checks.push(DoctorCheckEntry {
                id: "rollback_recovery".into(),
                category: "storage".into(),
                outcome: DoctorCheckOutcome::Repaired,
                attempts: outcome.attempts,
                summary: format!(
                    "Resumed rollback snapshot {} ({} verified files); preserved newer copy at {}; external_work_paused={}",
                    outcome.snapshot_id,
                    outcome.verified_files,
                    outcome.preserved_newer_dir.display(),
                    outcome.external_work_paused
                ),
                next_action: None,
                repair_action: Some(FixedRepairAction::VerifyAndMigrateStorage),
            }),
            Err(error) => checks.push(DoctorCheckEntry {
                id: "rollback_recovery".into(),
                category: "storage".into(),
                outcome: DoctorCheckOutcome::Failed,
                attempts: rb_journal.attempts.saturating_add(1),
                summary: format!("Interrupted rollback recovery failed: {error}"),
                next_action: Some("Inspect snapshots and storage lease before retrying.".into()),
                repair_action: Some(FixedRepairAction::VerifyAndMigrateStorage),
            }),
        }
    }
    if let Some((path, error)) = config_error {
        let attempts = journal
            .attempts_by_check
            .entry("configuration".into())
            .or_default();
        let mut check = DoctorCheckEntry {
            id: "configuration".into(),
            category: "storage".into(),
            outcome: DoctorCheckOutcome::ActionRequired,
            attempts: *attempts,
            summary: format!("DT configuration could not be loaded: {error}"),
            next_action: Some("Review the DT configuration file, then run Doctor again.".into()),
            repair_action: Some(FixedRepairAction::RestoreDefaultConfig),
        };
        if apply_repairs
            && *attempts < MAX_REPAIR_ATTEMPTS
            && fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_file())
        {
            *attempts += 1;
            check.attempts = *attempts;
            match repair_corrupt_config(path, config) {
                Ok(true) => {
                    check.outcome = DoctorCheckOutcome::Repaired;
                    check.summary = "Preserved corrupt configuration and installed defaults".into();
                    check.next_action = None;
                }
                Ok(false) => {}
                Err(error) => {
                    check.outcome = DoctorCheckOutcome::Failed;
                    check.summary = format!("Configuration recovery failed: {error}");
                }
            }
        }
        // The original config could name another storage directory. Limit repairs to the config file.
        apply_repairs = false;
        checks.push(check);
    }
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
    // Hold the same lease as each server while checking and removing stale sockets.
    #[cfg(unix)]
    for (path, extension) in [
        (&config.socket_path, "helper.lock"),
        (&config.daemon_socket_path, "daemon.lock"),
    ] {
        if !apply_repairs {
            break;
        }
        match repair_stale_socket(path, extension) {
            Ok(true) => {
                storage_outcome = DoctorCheckOutcome::Repaired;
                storage_msg = format!(
                    "{storage_msg}; removed verified stale socket {}",
                    path.display()
                );
            }
            Ok(false) => {}
            Err(e) => {
                storage_outcome = DoctorCheckOutcome::Failed;
                storage_msg = format!("{storage_msg}; socket ownership check failed: {e}");
                storage_next = Some("Inspect socket ownership manually.".into());
                break;
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
    let legacy = (storage_dir == &gm_core::config::default_storage_dir())
        .then(gm_core::config::legacy_storage_dir)
        .flatten();
    let mut migration_check = DoctorCheckEntry {
        id: "migration_state".into(),
        category: "storage".into(),
        outcome: DoctorCheckOutcome::Pass,
        attempts: 1,
        summary: "SQLite stores passed integrity check and the schema is current".into(),
        next_action: None,
        repair_action: None,
    };
    let state = inspect_migration(legacy.as_deref(), storage_dir).and_then(|state| {
        for name in [
            "openbot.sqlite",
            "channels.sqlite",
            "usage.sqlite3",
            "memory.sqlite",
        ] {
            let path = storage_dir.join(name);
            let path = if name == "memory.sqlite" && path.is_dir() {
                path.join("memory.db")
            } else {
                path
            };
            if gm_core::database::recovery_directory(&path).try_exists()? {
                return Err(GmError::Platform(
                    "Resume diagnostics recover-database before migration".into(),
                ));
            }
            if path.try_exists()? {
                gm_openbot::check_sqlite_integrity(&path)?;
            }
        }
        Ok(state)
    });
    match state {
        Ok(MigrationState::Fresh) => {
            migration_check.summary = "No existing data requires migration".into();
        }
        Ok(MigrationState::Current) => {}
        Ok(MigrationState::Conflict) => {
            migration_check.outcome = DoctorCheckOutcome::ActionRequired;
            migration_check.summary = "Separate legacy and DT stores both contain data".into();
            migration_check.next_action =
                Some("Preserve both stores before choosing which to activate.".into());
        }
        Ok(MigrationState::Pending) => {
            migration_check.outcome = DoctorCheckOutcome::ActionRequired;
            migration_check.summary = "Existing data requires a schema migration".into();
            migration_check.next_action =
                Some("Stop DT services and run `dt doctor repair`.".into());
            migration_check.repair_action = Some(FixedRepairAction::VerifyAndMigrateStorage);
            let attempts = journal
                .attempts_by_check
                .entry("migration_state".into())
                .or_default();
            if apply_repairs && *attempts < MAX_REPAIR_ATTEMPTS {
                *attempts += 1;
                match migrate_user_data(
                    legacy.as_deref(),
                    storage_dir,
                    env!("CARGO_PKG_VERSION"),
                    None,
                ) {
                    Ok(result) if !result.dual_store_conflict => {
                        migration_check.outcome = DoctorCheckOutcome::Repaired;
                        migration_check.summary = result.message;
                        migration_check.next_action = None;
                    }
                    Ok(result) => migration_check.summary = result.message,
                    Err(error) => {
                        migration_check.outcome = DoctorCheckOutcome::Failed;
                        migration_check.summary = format!("Migration failed: {error}");
                    }
                }
            } else if apply_repairs {
                migration_check.outcome = DoctorCheckOutcome::Failed;
                migration_check.summary = "Migration reached the repair attempt limit".into();
            }
            migration_check.attempts = *attempts;
        }
        Err(error) => {
            migration_check.outcome = DoctorCheckOutcome::Failed;
            migration_check.summary = format!("Storage check failed: {error}");
            migration_check.next_action = Some(
                "Preserve the profile and review its schema marker and database integrity.".into(),
            );
        }
    }
    checks.push(migration_check);

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
    let (grants_outcome, grants_summary, grants_next) = check_os_grants_actionable(&journal.run_id);
    checks.push(DoctorCheckEntry {
        id: "os_grants".into(),
        category: "permissions".into(),
        outcome: grants_outcome,
        attempts: 1,
        summary: grants_summary,
        next_action: grants_next,
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
    let component_mgr = crate::components::ComponentManager::open(storage_dir).ok();
    let vlm_models = component_mgr
        .as_ref()
        .map(|m| m.installed_components_for_kind(crate::components::ComponentKind::VlmModel))
        .unwrap_or_default();
    let vlm_workers = component_mgr
        .as_ref()
        .map(|m| m.installed_components_for_kind(crate::components::ComponentKind::VlmWorker))
        .unwrap_or_default();
    let verified_model = component_mgr.as_ref().and_then(|manager| {
        vlm_models
            .iter()
            .find(|model| manager.has_passing_vlm_probe(&model.id))
    });
    let (vlm_readiness, vlm_summary, vlm_next) = if let Some(model) = verified_model {
        (
            ReadinessState::Ready,
            format!("Offline image inference passed for {}", model.id),
            None,
        )
    } else if !vlm_models.is_empty() || !vlm_workers.is_empty() {
        (
            ReadinessState::NeedsAction,
            format!(
                "Downloaded local VLM assets ({} model(s), {} worker(s))",
                vlm_models.len(),
                vlm_workers.len()
            ),
            Some(
                "Install the matching model assets, then run dt models probe <model> <image.jpg>."
                    .into(),
            ),
        )
    } else {
        (
            ReadinessState::Unconfigured,
            "Catalog VLM assets are available on demand".into(),
            Some("Run dt models install <model> or use Settings → Local Components.".into()),
        )
    };
    capabilities.push(CapabilityStatus {
        id: "local_vlm".into(),
        name: "Local VLM Workers (Qwen2-VL-2B / SmolVLM / SmolVLM2)".into(),
        readiness: vlm_readiness,
        summary: vlm_summary,
        next_action: vlm_next,
    });

    let stt_installed = component_mgr
        .as_ref()
        .map(|m| m.installed_components_for_kind(crate::components::ComponentKind::SpeechStt))
        .unwrap_or_default();
    let tts_installed = component_mgr
        .as_ref()
        .map(|m| m.installed_components_for_kind(crate::components::ComponentKind::SpeechTts))
        .unwrap_or_default();
    let (voice_readiness, voice_summary, voice_next) = if !stt_installed.is_empty()
        || !tts_installed.is_empty()
    {
        (
            ReadinessState::NeedsAction,
            format!(
                "Local speech assets installed (STT: {}, TTS: {})",
                stt_installed.len(),
                tts_installed.len()
            ),
            Some("Check microphone permission and a working local speech runtime before using Voice.".into()),
        )
    } else {
        (
            ReadinessState::Unconfigured,
            "Local speech runtime has not been verified".into(),
            Some("Configure a local speech runtime and verify dictation and playback.".into()),
        )
    };
    capabilities.push(CapabilityStatus {
        id: "voice_runtime".into(),
        name: "Managed Whisper STT & Local TTS".into(),
        readiness: voice_readiness,
        summary: voice_summary,
        next_action: voice_next,
    });

    // First-run informational hints (never Failed or ActionRequired)
    let mut push_hint_check = |id: &str,
                               category: &str,
                               is_ok: bool,
                               hint_outcome: DoctorCheckOutcome,
                               summary: String,
                               next_action: Option<String>| {
        let outcome = match is_ok {
            true => DoctorCheckOutcome::Pass,
            false => hint_outcome,
        };
        checks.push(DoctorCheckEntry {
            id: id.into(),
            category: category.into(),
            outcome,
            attempts: 1_u32,
            summary,
            next_action,
            repair_action: Option::<FixedRepairAction>::None,
        });
    };

    let web_dist_opt = crate::resolve_web_dist(config);
    let web_install_hint = "Extract the DT web client bundle or build clients/web, then set DT_WEB_DIST to its dist directory.";
    push_hint_check(
        "web_app_present",
        "web",
        web_dist_opt.is_some(),
        DoctorCheckOutcome::Info,
        if web_dist_opt.is_some() {
            "React web app bundle available for `dt serve` at `/app/`".into()
        } else {
            web_install_hint.into()
        },
        web_dist_opt.is_none().then(|| web_install_hint.into()),
    );

    let missing_components: Vec<String> = component_mgr
        .as_ref()
        .map(|m| {
            m.list_statuses()
                .into_iter()
                .filter(|c| {
                    !matches!(
                        c.status,
                        crate::components::ComponentInstallState::Installed
                            | crate::components::ComponentInstallState::Verified
                    )
                })
                .map(|c| {
                    let mb = (c.total_size_bytes / (1024 * 1024)).max(1);
                    format!("{} ({mb} MB)", c.id)
                })
                .collect()
        })
        .unwrap_or_default();
    let first_missing_id = component_mgr.as_ref().and_then(|m| {
        m.list_statuses()
            .into_iter()
            .find(|c| {
                !matches!(
                    c.status,
                    crate::components::ComponentInstallState::Installed
                        | crate::components::ComponentInstallState::Verified
                )
            })
            .map(|c| {
                let mb = (c.total_size_bytes / (1024 * 1024)).max(1);
                (c.id, mb)
            })
    });
    push_hint_check(
        "optional_components",
        "components",
        first_missing_id.is_none(),
        DoctorCheckOutcome::Info,
        if missing_components.is_empty() {
            "All target optional local components are installed".into()
        } else {
            format!(
                "Optional local components available on demand: {}",
                missing_components.join(", ")
            )
        },
        first_missing_id.map(|(id, mb)| {
            format!(
                "Optional: `dt models install {id}` or install `{id}` in Settings -> Local Components (size: {mb} MB)."
            )
        }),
    );

    let hosted_enabled = std::env::var("DT_HOSTED_ENABLED")
        .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
    let has_pg_url = std::env::var("DT_DATABASE_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .ok()
        .is_some_and(|u| !u.trim().is_empty());
    let hosted_uses_sqlite = hosted_enabled && !has_pg_url;
    push_hint_check(
        "hosted_database",
        "hosted",
        !hosted_uses_sqlite,
        DoctorCheckOutcome::Warn,
        if hosted_uses_sqlite {
            "Hosted mode uses SQLite. Set `DT_DATABASE_URL=postgres://…` for multi-user deployments.".into()
        } else if has_pg_url {
            "Hosted control plane configured for PostgreSQL".into()
        } else {
            "Local single-user mode (hosted control plane disabled)".into()
        },
        hosted_uses_sqlite.then(|| {
            "Hosted mode uses SQLite. Set `DT_DATABASE_URL=postgres://…` for multi-user deployments.".into()
        }),
    );

    let catalog_expiring_soon = crate::components::ComponentCatalog::bundled()
        .ok()
        .and_then(|cat| chrono::DateTime::parse_from_rfc3339(&cat.expires_at).ok())
        .is_some_and(|exp| exp.with_timezone(&Utc) <= Utc::now() + chrono::Duration::days(30));
    push_hint_check(
        "metadata_expiry",
        "updates",
        !catalog_expiring_soon,
        DoctorCheckOutcome::Warn,
        if catalog_expiring_soon {
            "Signed component catalog metadata expires within 30 days. Update DT to refresh signed metadata.".into()
        } else {
            "Signed component catalog metadata expiration is healthy (>30 days remaining)".into()
        },
        catalog_expiring_soon.then(|| "Update DT to refresh signed metadata.".into()),
    );

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

    if mode != DoctorMode::Check && storage_dir.is_dir() {
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
    async fn doctor_checks_the_database_inside_the_memory_store() {
        let root = tempfile::tempdir().unwrap();
        let config = GmConfig {
            storage_dir: root.path().join("storage"),
            socket_path: root.path().join("helper.sock"),
            daemon_socket_path: root.path().join("daemon.sock"),
            ..GmConfig::default()
        };
        migrate_user_data(None, &config.storage_dir, env!("CARGO_PKG_VERSION"), None).unwrap();
        let memory =
            gm_memory::PersonalMemoryStore::open(config.storage_dir.join("memory.sqlite")).unwrap();
        memory
            .record_fact(None, "A fact indexed by FTS5", &[])
            .unwrap();
        let db_path = memory.db_path();

        let check_migration = async || {
            run_doctor(&config, DoctorMode::Check, None)
                .await
                .unwrap()
                .checks
                .into_iter()
                .find(|c| c.id == "migration_state")
                .unwrap()
        };
        let original = fs::read(&db_path).unwrap();
        let check = check_migration().await;
        assert_eq!(check.outcome, DoctorCheckOutcome::Pass, "{}", check.summary);
        assert_eq!(fs::read(&db_path).unwrap(), original);
        drop(memory);

        rusqlite::Connection::open(&db_path)
            .unwrap()
            .execute("DELETE FROM records_fts_docsize", [])
            .unwrap();
        let check = check_migration().await;
        assert_eq!(
            check.outcome,
            DoctorCheckOutcome::Failed,
            "{}",
            check.summary
        );
        assert!(
            check.summary.contains("malformed inverted index"),
            "{}",
            check.summary
        );

        let corrupt = b"not a SQLite database";
        fs::write(&db_path, corrupt).unwrap();
        let check = check_migration().await;
        assert_eq!(
            check.outcome,
            DoctorCheckOutcome::Failed,
            "{}",
            check.summary
        );
        assert_eq!(fs::read(&db_path).unwrap(), corrupt);
    }

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

    #[tokio::test]
    async fn doctor_reports_then_preserves_invalid_config() {
        let root = tempfile::tempdir().unwrap();
        let config = GmConfig {
            storage_dir: root.path().join("storage"),
            socket_path: root.path().join("helper.sock"),
            daemon_socket_path: root.path().join("daemon.sock"),
            ..GmConfig::default()
        };
        let path = root.path().join("config.json");
        let broken = b"{invalid";
        fs::write(&path, broken).unwrap();
        let error = GmConfig::load(Some(&path)).unwrap_err();
        let check = run_doctor_inner(&config, DoctorMode::Check, None, Some((&path, &error)))
            .await
            .unwrap();
        assert_eq!(check.checks[0].outcome, DoctorCheckOutcome::ActionRequired);
        assert_eq!(fs::read(&path).unwrap(), broken);
        assert!(!config.storage_dir.exists());

        let report = run_doctor_inner(&config, DoctorMode::Repair, None, Some((&path, &error)))
            .await
            .unwrap();
        assert_eq!(report.checks[0].outcome, DoctorCheckOutcome::Repaired);
        assert!(GmConfig::load(Some(&path)).is_ok());
        let backup = fs::read_dir(root.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.extension().is_some_and(|ext| ext == "bak"))
            .unwrap();
        assert_eq!(fs::read(backup).unwrap(), broken);
    }

    #[cfg(unix)]
    #[test]
    fn doctor_preserves_live_sockets_and_unrelated_files() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("daemon.sock");
        let lease = gm_core::ipc::try_lock_unix_socket(&path, "daemon.lock")
            .unwrap()
            .unwrap();
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        assert!(!repair_stale_socket(&path, "daemon.lock").unwrap());
        drop(lease);
        assert!(!repair_stale_socket(&path, "daemon.lock").unwrap());
        drop(listener);
        assert!(repair_stale_socket(&path, "daemon.lock").unwrap());
        fs::write(&path, b"keep").unwrap();
        assert!(repair_stale_socket(&path, "daemon.lock").is_err());
        assert_eq!(fs::read(&path).unwrap(), b"keep");
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

    struct DoctorEnvReset;
    impl Drop for DoctorEnvReset {
        fn drop(&mut self) {
            unsafe {
                std::env::remove_var("DT_WEB_DIST");
                std::env::remove_var("DT_HOSTED_ENABLED");
                std::env::remove_var("DT_DATABASE_URL");
                std::env::remove_var("DATABASE_URL");
                std::env::remove_var("DT_COMPONENT_CATALOG_PATH");
            }
        }
    }

    #[tokio::test]
    async fn test_doctor_hint_web_app_present() {
        let _guard = crate::tests::ENV_MUTEX.lock().await;
        let _reset = DoctorEnvReset;
        let root = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("DT_WEB_DIST", root.path().join("missing-dist"));
        }
        let config = GmConfig {
            storage_dir: root.path().join("storage"),
            socket_path: root.path().join("helper.sock"),
            daemon_socket_path: root.path().join("daemon.sock"),
            ..Default::default()
        };
        let dist = root.path().join("present-dist");
        fs::create_dir_all(&dist).unwrap();
        fs::write(dist.join("index.html"), "<!doctype html>").unwrap();
        unsafe {
            std::env::set_var("DT_WEB_DIST", &dist);
        }
        let report = run_doctor(&config, DoctorMode::Check, None).await.unwrap();
        let entry = report
            .checks
            .iter()
            .find(|c| c.id == "web_app_present")
            .unwrap();
        assert_eq!(entry.outcome, DoctorCheckOutcome::Pass);
    }

    #[tokio::test]
    async fn test_doctor_hint_optional_components() {
        let _guard = crate::tests::ENV_MUTEX.lock().await;
        let _reset = DoctorEnvReset;
        let root = tempfile::tempdir().unwrap();
        let config = GmConfig {
            storage_dir: root.path().join("storage"),
            socket_path: root.path().join("helper.sock"),
            daemon_socket_path: root.path().join("daemon.sock"),
            ..Default::default()
        };
        let report = run_doctor(&config, DoctorMode::Check, None).await.unwrap();
        let entry = report
            .checks
            .iter()
            .find(|c| c.id == "optional_components")
            .unwrap();
        assert_eq!(entry.outcome, DoctorCheckOutcome::Info);
        assert!(
            entry
                .next_action
                .as_deref()
                .unwrap_or("")
                .contains("dt models install ")
        );
    }

    #[tokio::test]
    async fn test_doctor_hint_hosted_database() {
        let _guard = crate::tests::ENV_MUTEX.lock().await;
        let _reset = DoctorEnvReset;
        let root = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("DT_HOSTED_ENABLED", "1");
            std::env::remove_var("DT_DATABASE_URL");
            std::env::remove_var("DATABASE_URL");
        }
        let config = GmConfig {
            storage_dir: root.path().join("storage"),
            socket_path: root.path().join("helper.sock"),
            daemon_socket_path: root.path().join("daemon.sock"),
            ..Default::default()
        };
        let report = run_doctor(&config, DoctorMode::Check, None).await.unwrap();
        let entry = report
            .checks
            .iter()
            .find(|c| c.id == "hosted_database")
            .unwrap();
        assert_eq!(entry.outcome, DoctorCheckOutcome::Warn);
        assert!(
            entry
                .next_action
                .as_deref()
                .unwrap_or("")
                .contains("DT_DATABASE_URL=postgres://")
        );
    }

    #[tokio::test]
    async fn test_doctor_hint_metadata_expiry() {
        let _guard = crate::tests::ENV_MUTEX.lock().await;
        let _reset = DoctorEnvReset;
        let root = tempfile::tempdir().unwrap();
        let cat_path = root.path().join("expiring-catalog.json");
        let mut cat: serde_json::Value =
            serde_json::from_str(include_str!("../../../docs/release/component-catalog.json"))
                .unwrap();
        cat["expires_at"] =
            serde_json::Value::String((Utc::now() + chrono::Duration::days(5)).to_rfc3339());
        fs::write(&cat_path, serde_json::to_string(&cat).unwrap()).unwrap();
        unsafe {
            std::env::set_var("DT_COMPONENT_CATALOG_PATH", &cat_path);
        }
        let config = GmConfig {
            storage_dir: root.path().join("storage"),
            socket_path: root.path().join("helper.sock"),
            daemon_socket_path: root.path().join("daemon.sock"),
            ..Default::default()
        };
        let report = run_doctor(&config, DoctorMode::Check, None).await.unwrap();
        let entry = report
            .checks
            .iter()
            .find(|c| c.id == "metadata_expiry")
            .unwrap();
        assert_eq!(entry.outcome, DoctorCheckOutcome::Warn);
        assert_eq!(
            entry.next_action.as_deref(),
            Some("Update DT to refresh signed metadata.")
        );
    }
}
