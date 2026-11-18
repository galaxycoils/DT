//! Versioned data migration, consistent SQLite snapshotting, dual-store conflict preservation,
//! monotonic safety preservation, and journaled package+data rollback for DT v1.0.1.

use chrono::Utc;
use gm_core::error::{GmError, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::path::{Component, Path, PathBuf};

pub const CURRENT_DATA_SCHEMA_VERSION: u32 = 2;
pub const MAX_ROLLBACK_ATTEMPTS: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DualStoreChoice {
    PreserveLegacy,
    PreserveDt,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SnapshotFileRecord {
    pub relative_path: String,
    pub sha256: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RollbackSnapshot {
    pub schema: u32,
    pub snapshot_id: String,
    pub package_version: String,
    #[serde(default)]
    pub package_digest: Option<String>,
    pub data_schema_version: u32,
    pub created_at: String,
    pub source_storage_dir: PathBuf,
    pub snapshot_dir: PathBuf,
    pub files: Vec<SnapshotFileRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MigrationOutcome {
    pub status: String,
    pub from_schema: u32,
    pub to_schema: u32,
    pub snapshot_id: Option<String>,
    pub dual_store_conflict: bool,
    pub message: String,
}

/// Monotonic safety records persisted outside the rollback set so restoring an older
/// snapshot can never reactivate revoked credentials, spent budgets, forgotten memory,
/// withdrawn versions, or paused recording.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct MonotonicSafetyState {
    pub schema: u32,
    pub revoked_credential_ids: BTreeSet<String>,
    pub spent_budget_micros: u64,
    pub exhausted_budget_scopes: BTreeSet<String>,
    pub forgotten_memory_fingerprints: BTreeSet<String>,
    pub revoked_update_versions: BTreeSet<String>,
    pub paused_recording: bool,
    pub held_external_jobs: BTreeSet<String>,
    pub updated_at: String,
}

impl MonotonicSafetyState {
    pub fn load(storage_dir: &Path) -> Result<Self> {
        let path = storage_dir.join("monotonic-safety.json");
        match fs::read_to_string(&path) {
            Ok(raw) => serde_json::from_str(&raw)
                .map_err(|e| GmError::Config(format!("Corrupt monotonic safety state: {e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self {
                schema: 1,
                ..Default::default()
            }),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&mut self, storage_dir: &Path) -> Result<()> {
        self.schema = 1;
        self.updated_at = Utc::now().to_rfc3339();
        let bytes = serde_json::to_vec_pretty(self)?;
        gm_core::storage::atomic_write_private(&storage_dir.join("monotonic-safety.json"), &bytes)
    }

    pub fn merge_from(&mut self, other: &Self) {
        self.revoked_credential_ids
            .extend(other.revoked_credential_ids.iter().cloned());
        self.spent_budget_micros = self.spent_budget_micros.max(other.spent_budget_micros);
        self.exhausted_budget_scopes
            .extend(other.exhausted_budget_scopes.iter().cloned());
        self.forgotten_memory_fingerprints
            .extend(other.forgotten_memory_fingerprints.iter().cloned());
        self.revoked_update_versions
            .extend(other.revoked_update_versions.iter().cloned());
        self.paused_recording = self.paused_recording || other.paused_recording;
        self.held_external_jobs
            .extend(other.held_external_jobs.iter().cloned());
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RollbackPhase {
    Planned,
    PreservedNewer,
    PackageSwitched,
    DataRestored,
    Verified,
    Completed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RollbackJournal {
    pub schema: u32,
    pub run_id: String,
    pub snapshot_dir: PathBuf,
    pub target_storage_dir: PathBuf,
    pub preserved_newer_dir: PathBuf,
    pub phase: RollbackPhase,
    pub attempts: u32,
    pub external_work_paused: bool,
    pub disconnected_recovery: bool,
    pub target_package_version: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RollbackOutcome {
    pub run_id: String,
    pub snapshot_id: String,
    pub preserved_newer_dir: PathBuf,
    pub phase: RollbackPhase,
    pub attempts: u32,
    pub external_work_paused: bool,
    pub disconnected_recovery: bool,
    pub verified_files: usize,
}

/// Exclusive advisory lease protecting storage directory during migration, backup, or rollback.
pub struct StorageLease {
    _file: File,
    pub lock_path: PathBuf,
}

impl StorageLease {
    pub fn acquire(storage_dir: &Path) -> Result<Self> {
        gm_core::storage::ensure_private_dir(storage_dir)?;
        let lock_path = storage_dir.join(".dt-storage.lease");
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)?;
        file.try_lock().map_err(|e| {
            GmError::Platform(format!(
                "Cannot acquire exclusive storage lease at {}: another DT writer is active ({e})",
                lock_path.display()
            ))
        })?;
        Ok(Self {
            _file: file,
            lock_path,
        })
    }
}

fn sha256_file(path: &Path) -> Result<(String, u64)> {
    let bytes = fs::read(path)?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok((hex::encode(hasher.finalize()), bytes.len() as u64))
}

fn current_executable_digest() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let (digest, _) = sha256_file(&exe).ok()?;
    Some(digest)
}

fn validate_safe_relative_path(rel: &str) -> Result<PathBuf> {
    if rel.is_empty() || rel.contains('\\') {
        return Err(GmError::SafetyViolation(format!(
            "Unsafe snapshot relative path '{rel}'"
        )));
    }
    let path = Path::new(rel);
    if path.is_absolute() {
        return Err(GmError::SafetyViolation(format!(
            "Snapshot path must be relative: '{rel}'"
        )));
    }
    let mut clean = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::Normal(part) => clean.push(part),
            _ => {
                return Err(GmError::SafetyViolation(format!(
                    "Snapshot path traversal rejected: '{rel}'"
                )));
            }
        }
    }
    Ok(clean)
}

fn is_sqlite_name(rel: &str) -> bool {
    rel.ends_with(".sqlite") || rel.ends_with(".sqlite3")
}

fn check_sqlite_quick(path: &Path) -> Result<()> {
    let conn = Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| GmError::Platform(format!("Cannot reopen SQLite {}: {e}", path.display())))?;
    let status: String = conn
        .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
        .map_err(|e| GmError::Platform(format!("SQLite quick_check failed: {e}")))?;
    if !status.eq_ignore_ascii_case("ok") {
        return Err(GmError::Platform(format!(
            "SQLite integrity check failed for {}: {status}",
            path.display()
        )));
    }
    Ok(())
}

pub(crate) fn available_disk_bytes(path: &Path) -> Option<u64> {
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

pub fn check_space_available(
    avail_bytes: Option<u64>,
    required_bytes: u64,
    context: &str,
) -> Result<()> {
    if let Some(avail) = avail_bytes
        && avail < required_bytes
    {
        return Err(GmError::Platform(format!(
            "Insufficient disk space for {context}: need {required_bytes} bytes, have {avail} bytes"
        )));
    }
    Ok(())
}

fn read_schema_marker(storage_dir: &Path) -> Result<Option<u32>> {
    let marker = storage_dir.join("dt-schema-version.json");
    let raw = match fs::read_to_string(&marker) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let val: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| GmError::Config(format!("Corrupt schema marker {}: {e}", marker.display())))?;
    let ver = val["schema_version"]
        .as_u64()
        .and_then(|v| u32::try_from(v).ok())
        .filter(|&v| v > 0)
        .ok_or_else(|| GmError::Config("Invalid schema_version in marker".into()))?;
    Ok(Some(ver))
}

fn write_schema_marker(storage_dir: &Path, version: u32, package_version: &str) -> Result<()> {
    let marker = storage_dir.join("dt-schema-version.json");
    let payload = serde_json::to_vec_pretty(&serde_json::json!({
        "schema_version": version,
        "package_version": package_version,
        "updated_at": Utc::now().to_rfc3339(),
    }))?;
    gm_core::storage::atomic_write_private(&marker, &payload)?;
    Ok(())
}

/// Snapshot a SQLite database consistently using SQLite `VACUUM INTO` and verify the copy.
pub fn backup_sqlite_consistent(src: &Path, dst: &Path) -> Result<()> {
    if let Some(parent) = dst.parent() {
        gm_core::storage::ensure_private_dir(parent)?;
    }
    if dst.exists() {
        fs::remove_file(dst)?;
    }
    let src_conn = Connection::open_with_flags(
        src,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| GmError::Platform(format!("Cannot open {} for backup: {e}", src.display())))?;
    let dst_str = dst
        .to_str()
        .ok_or_else(|| GmError::Platform("Non-UTF8 snapshot path".into()))?;
    src_conn
        .execute("VACUUM INTO ?1", [dst_str])
        .map_err(|e| GmError::Platform(format!("SQLite snapshot VACUUM INTO failed: {e}")))?;
    check_sqlite_quick(dst)?;
    Ok(())
}

fn should_skip_storage_entry(name: &str) -> bool {
    name.starts_with('.')
        || name.ends_with(".sock")
        || name.ends_with(".lock")
        || name.ends_with("-wal")
        || name.ends_with("-shm")
        || matches!(
            name,
            "dt-schema-version.json"
                | "dt-dual-store-choice.json"
                | "monotonic-safety.json"
                | "doctor-runs"
                | "snapshots"
        )
        || name.starts_with("preserved-newer-")
}

fn collect_snapshot_files_recursive(
    base: &Path,
    current: &Path,
    out: &mut Vec<PathBuf>,
) -> Result<()> {
    let entries = match fs::read_dir(current) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let meta = fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() {
            return Err(GmError::SafetyViolation(format!(
                "Refusing to follow symlink inside storage directory: {}",
                path.display()
            )));
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if current == base && should_skip_storage_entry(name) {
            continue;
        }
        if meta.is_dir() {
            collect_snapshot_files_recursive(base, &path, out)?;
        } else if meta.is_file() {
            let rel = path
                .strip_prefix(base)
                .map_err(|e| GmError::Platform(e.to_string()))?
                .to_path_buf();
            out.push(rel);
        }
    }
    Ok(())
}

fn dir_has_user_data(dir: &Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        !should_skip_storage_entry(&name)
    })
}

/// Snapshot all critical stores (including nested attachments, drafts, tasks, memory, audit, config)
/// under an exclusive storage lease before upgrade or rollback.
pub fn create_rollback_snapshot(
    storage_dir: &Path,
    package_version: &str,
    snapshots_root: &Path,
) -> Result<RollbackSnapshot> {
    let _lease = StorageLease::acquire(storage_dir)?;
    create_rollback_snapshot_unlocked(storage_dir, package_version, snapshots_root)
}

fn create_rollback_snapshot_unlocked(
    storage_dir: &Path,
    package_version: &str,
    snapshots_root: &Path,
) -> Result<RollbackSnapshot> {
    let schema_ver = read_schema_marker(storage_dir)?.unwrap_or(1);
    let mut rel_paths = Vec::new();
    collect_snapshot_files_recursive(storage_dir, storage_dir, &mut rel_paths)?;
    rel_paths.sort();

    let total_source_bytes: u64 = rel_paths
        .iter()
        .filter_map(|rel| fs::metadata(storage_dir.join(rel)).ok().map(|m| m.len()))
        .sum();
    check_space_available(
        available_disk_bytes(snapshots_root),
        total_source_bytes.saturating_add(1024 * 1024),
        "rollback snapshot",
    )?;

    let snapshot_id = format!(
        "snap-{}-{}",
        Utc::now().format("%Y%m%dT%H%M%SZ"),
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    let snapshot_dir = snapshots_root.join(&snapshot_id);
    gm_core::storage::ensure_private_dir(&snapshot_dir)?;

    let mut files = Vec::new();
    for rel in rel_paths {
        let rel_str = rel
            .to_str()
            .ok_or_else(|| GmError::Platform("Non-UTF8 relative path in storage".into()))?
            .replace('\\', "/");
        validate_safe_relative_path(&rel_str)?;
        let src = storage_dir.join(&rel);
        let dst = snapshot_dir.join(&rel);
        if let Some(parent) = dst.parent() {
            gm_core::storage::ensure_private_dir(parent)?;
        }
        if is_sqlite_name(&rel_str) {
            backup_sqlite_consistent(&src, &dst)?;
        } else {
            fs::copy(&src, &dst)?;
        }
        let (sha256, size_bytes) = sha256_file(&dst)?;
        files.push(SnapshotFileRecord {
            relative_path: rel_str,
            sha256,
            size_bytes,
        });
    }

    let snapshot = RollbackSnapshot {
        schema: 1,
        snapshot_id,
        package_version: package_version.to_string(),
        package_digest: current_executable_digest(),
        data_schema_version: schema_ver,
        created_at: Utc::now().to_rfc3339(),
        source_storage_dir: storage_dir.to_path_buf(),
        snapshot_dir: snapshot_dir.clone(),
        files,
    };
    let manifest_bytes = serde_json::to_vec_pretty(&snapshot)?;
    gm_core::storage::atomic_write_private(&snapshot_dir.join("rollback.json"), &manifest_bytes)?;
    Ok(snapshot)
}

pub fn list_rollback_snapshots(snapshots_root: &Path) -> Result<Vec<RollbackSnapshot>> {
    let entries = match fs::read_dir(snapshots_root) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let manifest_path = entry.path().join("rollback.json");
        if manifest_path.is_file()
            && let Ok(raw) = fs::read_to_string(&manifest_path)
            && let Ok(snap) = serde_json::from_str::<RollbackSnapshot>(&raw)
        {
            out.push(snap);
        }
    }
    out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(out)
}

pub fn inspect_rollback_snapshot(snapshot_dir: &Path) -> Result<RollbackSnapshot> {
    let manifest_raw = fs::read_to_string(snapshot_dir.join("rollback.json"))?;
    let snapshot: RollbackSnapshot = serde_json::from_str(&manifest_raw)?;
    for file in &snapshot.files {
        let rel = validate_safe_relative_path(&file.relative_path)?;
        let src = snapshot_dir.join(&rel);
        let meta = fs::symlink_metadata(&src)?;
        if meta.file_type().is_symlink() || !meta.is_file() {
            return Err(GmError::SafetyViolation(format!(
                "Snapshot entry {} is not a regular file",
                file.relative_path
            )));
        }
        let (actual_sha, actual_size) = sha256_file(&src)?;
        if actual_sha != file.sha256 || actual_size != file.size_bytes {
            return Err(GmError::SafetyViolation(format!(
                "Rollback snapshot file {} failed size/SHA-256 verification",
                file.relative_path
            )));
        }
    }
    Ok(snapshot)
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum MigrationState {
    Current,
    Fresh,
    Pending,
    Conflict,
}

fn canonical_or_parent_join(path: &Path) -> Option<PathBuf> {
    if let Ok(canon) = path.canonicalize() {
        return Some(canon);
    }
    let parent = path.parent()?.canonicalize().ok()?;
    Some(parent.join(path.file_name()?))
}

fn distinct_legacy<'a>(legacy: Option<&'a Path>, target: &Path) -> Option<&'a Path> {
    legacy.filter(|legacy| {
        *legacy != target
            && match (
                canonical_or_parent_join(legacy),
                canonical_or_parent_join(target),
            ) {
                (Some(left), Some(right)) => left != right,
                _ => true,
            }
    })
}

fn reject_downgrade(storage_dir: &Path) -> Result<Option<u32>> {
    let version = read_schema_marker(storage_dir)?;
    if let Some(version) = version
        && version > CURRENT_DATA_SCHEMA_VERSION
    {
        return Err(GmError::Config(format!(
            "Schema downgrade refused: store schema v{version} is newer than DT binary schema v{CURRENT_DATA_SCHEMA_VERSION}"
        )));
    }
    Ok(version)
}

pub(crate) fn inspect_migration(legacy: Option<&Path>, target: &Path) -> Result<MigrationState> {
    let version = reject_downgrade(target)?;
    if version == Some(CURRENT_DATA_SCHEMA_VERSION) {
        return Ok(MigrationState::Current);
    }
    let legacy = distinct_legacy(legacy, target).filter(|path| dir_has_user_data(path));
    if let Some(path) = legacy {
        reject_downgrade(path)?;
    }
    Ok(match (legacy.is_some(), dir_has_user_data(target)) {
        (true, true) => MigrationState::Conflict,
        (false, false) if version.is_none() => MigrationState::Fresh,
        _ => MigrationState::Pending,
    })
}

fn read_dual_store_choice(dt_storage_dir: &Path) -> Option<DualStoreChoice> {
    let raw = fs::read_to_string(dt_storage_dir.join("dt-dual-store-choice.json")).ok()?;
    let val: serde_json::Value = serde_json::from_str(&raw).ok()?;
    serde_json::from_value(val.get("choice")?.clone()).ok()
}

fn persist_dual_store_choice(dt_storage_dir: &Path, choice: DualStoreChoice) -> Result<()> {
    let payload = serde_json::to_vec_pretty(&serde_json::json!({
        "choice": choice,
        "recorded_at": Utc::now().to_rfc3339(),
    }))?;
    gm_core::storage::atomic_write_private(
        &dt_storage_dir.join("dt-dual-store-choice.json"),
        &payload,
    )
}

/// Migrate from v0.7.0/legacy `grok-muse` stores into `dt` while preserving custom paths,
/// resolving path aliases, preventing schema downgrades, and halting safely if both stores contain data.
pub fn migrate_user_data(
    legacy_dir: Option<&Path>,
    dt_storage_dir: &Path,
    package_version: &str,
    choice: Option<DualStoreChoice>,
) -> Result<MigrationOutcome> {
    let _lease = StorageLease::acquire(dt_storage_dir)?;
    let legacy_dir = distinct_legacy(legacy_dir, dt_storage_dir);
    let migration = inspect_migration(legacy_dir, dt_storage_dir)?;
    if migration == MigrationState::Current {
        return Ok(MigrationOutcome {
            status: "up_to_date".into(),
            from_schema: CURRENT_DATA_SCHEMA_VERSION,
            to_schema: CURRENT_DATA_SCHEMA_VERSION,
            snapshot_id: None,
            dual_store_conflict: false,
            message: "User data schema is already current".into(),
        });
    }

    let legacy_has_data = legacy_dir.is_some_and(dir_has_user_data);
    let dt_has_data = dir_has_user_data(dt_storage_dir);
    let effective_choice = choice.or_else(|| read_dual_store_choice(dt_storage_dir));

    if migration == MigrationState::Conflict && effective_choice.is_none() {
        return Ok(MigrationOutcome {
            status: "conflict_requires_choice".into(),
            from_schema: 1,
            to_schema: CURRENT_DATA_SCHEMA_VERSION,
            snapshot_id: None,
            dual_store_conflict: true,
            message: format!(
                "Both legacy store ({}) and DT store ({}) contain data; both preserved until you choose which to activate",
                legacy_dir
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
                dt_storage_dir.display()
            ),
        });
    }

    if let Some(chosen) = effective_choice
        && migration == MigrationState::Conflict
    {
        persist_dual_store_choice(dt_storage_dir, chosen)?;
    }

    let snapshots_root = dt_storage_dir.join("snapshots");
    let source_for_snapshot = match (legacy_dir, legacy_has_data, dt_has_data, effective_choice) {
        (Some(legacy), true, false, _)
        | (Some(legacy), true, true, Some(DualStoreChoice::PreserveLegacy)) => legacy,
        _ => dt_storage_dir,
    };

    let _source_lease = if source_for_snapshot != dt_storage_dir {
        Some(StorageLease::acquire(source_for_snapshot)?)
    } else {
        None
    };
    let from_schema = reject_downgrade(source_for_snapshot)?.unwrap_or(1);
    let snapshot = if dir_has_user_data(source_for_snapshot) {
        let snap =
            create_rollback_snapshot_unlocked(source_for_snapshot, "0.7.0", &snapshots_root)?;
        if source_for_snapshot != dt_storage_dir {
            let staging = dt_storage_dir.join(format!(".migration-stage-{}", snap.snapshot_id));
            gm_core::storage::ensure_private_dir(&staging)?;
            for file in &snap.files {
                let rel = validate_safe_relative_path(&file.relative_path)?;
                let src = snap.snapshot_dir.join(&rel);
                let dst = staging.join(&rel);
                if let Some(parent) = dst.parent() {
                    gm_core::storage::ensure_private_dir(parent)?;
                }
                fs::copy(&src, &dst)?;
                let (actual_sha, _) = sha256_file(&dst)?;
                if actual_sha != file.sha256 {
                    let _ = fs::remove_dir_all(&staging);
                    return Err(GmError::Platform(
                        "Staged migration verification failed checksum".into(),
                    ));
                }
            }
            for file in &snap.files {
                let rel = validate_safe_relative_path(&file.relative_path)?;
                let staged = staging.join(&rel);
                let target = dt_storage_dir.join(&rel);
                if let Some(parent) = target.parent() {
                    gm_core::storage::ensure_private_dir(parent)?;
                }
                if target.exists() {
                    let preserved = dt_storage_dir.join(format!(
                        "{}.pre-migration.{}.bak",
                        rel.file_name().and_then(|n| n.to_str()).unwrap_or("file"),
                        snap.snapshot_id
                    ));
                    fs::rename(&target, &preserved)?;
                }
                fs::rename(&staged, &target)?;
            }
            let _ = fs::remove_dir_all(&staging);
        }
        Some(snap.snapshot_id)
    } else {
        None
    };

    write_schema_marker(dt_storage_dir, CURRENT_DATA_SCHEMA_VERSION, package_version)?;
    Ok(MigrationOutcome {
        status: "migrated".into(),
        from_schema,
        to_schema: CURRENT_DATA_SCHEMA_VERSION,
        snapshot_id: snapshot,
        dual_store_conflict: false,
        message: "Verified migration snapshot created and schema updated".into(),
    })
}

fn rollback_journal_path(storage_dir: &Path, run_id: &str) -> PathBuf {
    storage_dir
        .join("doctor-runs")
        .join(format!("{run_id}.rollback.json"))
}

pub fn load_rollback_journal(storage_dir: &Path, run_id: &str) -> Result<Option<RollbackJournal>> {
    let path = rollback_journal_path(storage_dir, run_id);
    match fs::read_to_string(&path) {
        Ok(raw) => Ok(Some(serde_json::from_str(&raw)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn save_rollback_journal(storage_dir: &Path, journal: &RollbackJournal) -> Result<()> {
    let dir = storage_dir.join("doctor-runs");
    gm_core::storage::ensure_private_dir(&dir)?;
    let bytes = serde_json::to_vec_pretty(journal)?;
    gm_core::storage::atomic_write_private(
        &rollback_journal_path(storage_dir, &journal.run_id),
        &bytes,
    )
}

fn invalidate_restored_runtime_state(
    target_storage_dir: &Path,
    monotonic: &mut MonotonicSafetyState,
) -> Result<()> {
    let db_path = target_storage_dir.join("openbot.sqlite");
    if db_path.is_file() {
        let conn = Connection::open(&db_path)
            .map_err(|e| GmError::Platform(format!("Cannot open restored openbot.sqlite: {e}")))?;
        // Invalidate restored sessions and hold pending approvals / in-flight external jobs.
        let has_sessions: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='hosted_sessions_v1')",
                [],
                |row| row.get(0),
            )
            .unwrap_or(false);
        if has_sessions {
            let _ = conn.execute(
                "UPDATE hosted_sessions_v1 SET revoked_at = COALESCE(revoked_at, ?1)",
                [Utc::now().to_rfc3339()],
            );
        }
        let has_tasks: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='bot_tasks')",
                [],
                |row| row.get(0),
            )
            .unwrap_or(false);
        if has_tasks {
            let mut stmt = conn
                .prepare("SELECT id FROM bot_tasks WHERE status IN ('running', 'waiting_approval', 'queued')")
                .map_err(|e| GmError::Platform(e.to_string()))?;
            let ids: Vec<String> = stmt
                .query_map([], |row| row.get(0))
                .map_err(|e| GmError::Platform(e.to_string()))?
                .flatten()
                .collect();
            for id in ids {
                monotonic.held_external_jobs.insert(id);
            }
            let _ = conn.execute(
                "UPDATE bot_tasks SET status = 'blocked', blocker_note = 'Held for reconciliation after rollback restore' WHERE status IN ('running', 'waiting_approval')",
                [],
            );
        }
    }
    monotonic.paused_recording = true;
    monotonic.save(target_storage_dir)?;
    Ok(())
}

/// Begin or resume a journaled rollback restore while preserving newer data and monotonic safety controls.
pub fn execute_journaled_rollback(
    snapshot_dir: &Path,
    target_storage_dir: &Path,
    run_id: &str,
) -> Result<RollbackOutcome> {
    let _lease = StorageLease::acquire(target_storage_dir)?;
    let snapshot = inspect_rollback_snapshot(snapshot_dir)?;

    let mut journal = match load_rollback_journal(target_storage_dir, run_id)? {
        Some(existing) => existing,
        None => {
            let preserved_newer_dir = target_storage_dir.join(format!(
                "preserved-newer-{}-{}",
                Utc::now().format("%Y%m%dT%H%M%SZ"),
                &uuid::Uuid::new_v4().simple().to_string()[..8]
            ));
            let disconnected = snapshot.data_schema_version < CURRENT_DATA_SCHEMA_VERSION
                && snapshot.package_version.starts_with("0.");
            let j = RollbackJournal {
                schema: 1,
                run_id: run_id.to_string(),
                snapshot_dir: snapshot_dir.to_path_buf(),
                target_storage_dir: target_storage_dir.to_path_buf(),
                preserved_newer_dir,
                phase: RollbackPhase::Planned,
                attempts: 0,
                external_work_paused: true,
                disconnected_recovery: disconnected,
                target_package_version: snapshot.package_version.clone(),
                updated_at: Utc::now().to_rfc3339(),
            };
            save_rollback_journal(target_storage_dir, &j)?;
            j
        }
    };

    if journal.phase == RollbackPhase::Completed {
        return Ok(RollbackOutcome {
            run_id: journal.run_id,
            snapshot_id: snapshot.snapshot_id,
            preserved_newer_dir: journal.preserved_newer_dir,
            phase: journal.phase,
            attempts: journal.attempts,
            external_work_paused: journal.external_work_paused,
            disconnected_recovery: journal.disconnected_recovery,
            verified_files: snapshot.files.len(),
        });
    }

    if journal.attempts >= MAX_ROLLBACK_ATTEMPTS {
        return Err(GmError::Platform(format!(
            "Rollback run '{run_id}' exceeded maximum {MAX_ROLLBACK_ATTEMPTS} attempts"
        )));
    }
    journal.attempts += 1;
    journal.updated_at = Utc::now().to_rfc3339();
    save_rollback_journal(target_storage_dir, &journal)?;

    // Capture monotonic safety records BEFORE touching target storage.
    let mut monotonic = MonotonicSafetyState::load(target_storage_dir)?;

    if journal.phase == RollbackPhase::Planned {
        let mut current_files = Vec::new();
        collect_snapshot_files_recursive(
            target_storage_dir,
            target_storage_dir,
            &mut current_files,
        )?;
        let required_bytes: u64 = current_files
            .iter()
            .filter_map(|rel| {
                fs::metadata(target_storage_dir.join(rel))
                    .ok()
                    .map(|m| m.len())
            })
            .sum::<u64>()
            .saturating_add(snapshot.files.iter().map(|f| f.size_bytes).sum::<u64>())
            .saturating_add(1024 * 1024);
        check_space_available(
            available_disk_bytes(target_storage_dir),
            required_bytes,
            "rollback restore",
        )?;

        gm_core::storage::ensure_private_dir(&journal.preserved_newer_dir)?;
        for rel in &current_files {
            let src = target_storage_dir.join(rel);
            let dst = journal.preserved_newer_dir.join(rel);
            if let Some(parent) = dst.parent() {
                gm_core::storage::ensure_private_dir(parent)?;
            }
            if src
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(is_sqlite_name)
            {
                backup_sqlite_consistent(&src, &dst)?;
            } else {
                fs::copy(&src, &dst)?;
            }
        }
        journal.phase = RollbackPhase::PreservedNewer;
        journal.updated_at = Utc::now().to_rfc3339();
        save_rollback_journal(target_storage_dir, &journal)?;
    }

    if matches!(
        journal.phase,
        RollbackPhase::PreservedNewer | RollbackPhase::PackageSwitched
    ) {
        // Stage on destination filesystem before moving into active slots.
        let stage_dir = target_storage_dir.join(format!(".restore-stage-{run_id}"));
        if stage_dir.exists() {
            fs::remove_dir_all(&stage_dir)?;
        }
        gm_core::storage::ensure_private_dir(&stage_dir)?;
        for file in &snapshot.files {
            let rel = validate_safe_relative_path(&file.relative_path)?;
            let src = snapshot_dir.join(&rel);
            let staged = stage_dir.join(&rel);
            if let Some(parent) = staged.parent() {
                gm_core::storage::ensure_private_dir(parent)?;
            }
            fs::copy(&src, &staged)?;
            let (actual_sha, actual_size) = sha256_file(&staged)?;
            if actual_sha != file.sha256 || actual_size != file.size_bytes {
                let _ = fs::remove_dir_all(&stage_dir);
                return Err(GmError::SafetyViolation(format!(
                    "Staged restore verification failed for {}",
                    file.relative_path
                )));
            }
        }

        // Remove older live files outside preserved/system dirs so stale newer records don't bleed in.
        let mut live_files = Vec::new();
        collect_snapshot_files_recursive(target_storage_dir, target_storage_dir, &mut live_files)?;
        for rel in live_files {
            let p = target_storage_dir.join(&rel);
            if p.is_file() {
                let _ = fs::remove_file(&p);
            }
        }
        for file in &snapshot.files {
            let rel = validate_safe_relative_path(&file.relative_path)?;
            let staged = stage_dir.join(&rel);
            let dst = target_storage_dir.join(&rel);
            if let Some(parent) = dst.parent() {
                gm_core::storage::ensure_private_dir(parent)?;
            }
            fs::rename(&staged, &dst)?;
        }
        let _ = fs::remove_dir_all(&stage_dir);

        write_schema_marker(
            target_storage_dir,
            snapshot.data_schema_version,
            &snapshot.package_version,
        )?;
        journal.phase = RollbackPhase::DataRestored;
        journal.updated_at = Utc::now().to_rfc3339();
        save_rollback_journal(target_storage_dir, &journal)?;
    }

    if journal.phase == RollbackPhase::DataRestored {
        for file in &snapshot.files {
            let rel = validate_safe_relative_path(&file.relative_path)?;
            let dst = target_storage_dir.join(&rel);
            let (actual_sha, actual_size) = sha256_file(&dst)?;
            if actual_sha != file.sha256 || actual_size != file.size_bytes {
                return Err(GmError::SafetyViolation(format!(
                    "Restored file verification failed for {}",
                    file.relative_path
                )));
            }
            if is_sqlite_name(&file.relative_path) {
                check_sqlite_quick(&dst)?;
            }
        }
        invalidate_restored_runtime_state(target_storage_dir, &mut monotonic)?;
        journal.phase = RollbackPhase::Verified;
        journal.updated_at = Utc::now().to_rfc3339();
        save_rollback_journal(target_storage_dir, &journal)?;
    }

    journal.phase = RollbackPhase::Completed;
    journal.updated_at = Utc::now().to_rfc3339();
    save_rollback_journal(target_storage_dir, &journal)?;

    Ok(RollbackOutcome {
        run_id: journal.run_id,
        snapshot_id: snapshot.snapshot_id,
        preserved_newer_dir: journal.preserved_newer_dir,
        phase: journal.phase,
        attempts: journal.attempts,
        external_work_paused: journal.external_work_paused,
        disconnected_recovery: journal.disconnected_recovery,
        verified_files: snapshot.files.len(),
    })
}

/// Restore a verified rollback snapshot while preserving newer post-upgrade data separately.
pub fn restore_rollback_snapshot(
    snapshot_dir: &Path,
    target_storage_dir: &Path,
) -> Result<PathBuf> {
    let run_id = format!("rb-{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);
    let outcome = execute_journaled_rollback(snapshot_dir, target_storage_dir, &run_id)?;
    Ok(outcome.preserved_newer_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_place_migration_is_pending_then_current_and_refuses_downgrades() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        fs::write(path.join("config.json"), b"{}").unwrap();
        assert_eq!(
            inspect_migration(Some(path), path).unwrap(),
            MigrationState::Pending
        );
        let outcome = migrate_user_data(Some(path), path, "1.0.1", None).unwrap();
        assert!(!outcome.dual_store_conflict);
        assert!(outcome.snapshot_id.is_some());
        assert_eq!(
            inspect_migration(Some(path), path).unwrap(),
            MigrationState::Current
        );
        write_schema_marker(path, 999, "9.0.0").unwrap();
        assert!(inspect_migration(Some(path), path).is_err());
    }

    #[test]
    fn test_migration_dual_store_conflict_and_rollback() {
        let root = std::env::temp_dir().join(format!("dt-mig-test-{}", uuid::Uuid::new_v4()));
        let legacy = root.join("grok-muse");
        let dt = root.join("dt");
        fs::create_dir_all(&legacy).unwrap();
        fs::create_dir_all(&dt).unwrap();

        let legacy_db = legacy.join("openbot.sqlite");
        let conn = Connection::open(&legacy_db).unwrap();
        conn.execute_batch("CREATE TABLE t (v TEXT); INSERT INTO t VALUES ('legacy');")
            .unwrap();
        drop(conn);
        fs::create_dir_all(legacy.join("attachments/sub")).unwrap();
        let attachment_bytes = b"nested-attachment-binary-payload-v1";
        fs::write(legacy.join("attachments/sub/note.bin"), attachment_bytes).unwrap();
        fs::write(legacy.join("config.json"), b"{\"legacy\":true}").unwrap();
        fs::write(dt.join("config.json"), b"{\"dt\":true}").unwrap();

        let conflict = migrate_user_data(Some(&legacy), &dt, "1.0.1", None).unwrap();
        assert!(conflict.dual_store_conflict);
        assert_eq!(conflict.status, "conflict_requires_choice");
        assert!(legacy_db.exists());
        assert!(dt.join("config.json").exists());

        let migrated = migrate_user_data(
            Some(&legacy),
            &dt,
            "1.0.1",
            Some(DualStoreChoice::PreserveLegacy),
        )
        .unwrap();
        assert_eq!(migrated.status, "migrated");
        let snap_id = migrated.snapshot_id.unwrap();
        assert_eq!(
            fs::read(dt.join("attachments/sub/note.bin")).unwrap(),
            attachment_bytes
        );

        // Simulate newer data + monotonic safety decisions after upgrade, then rollback.
        let mut safety = MonotonicSafetyState::load(&dt).unwrap();
        safety
            .revoked_credential_ids
            .insert("cred-revoked-1".into());
        safety.spent_budget_micros = 700_000;
        safety.forgotten_memory_fingerprints.insert("fp-123".into());
        safety.revoked_update_versions.insert("1.0.0-bad".into());
        safety.save(&dt).unwrap();

        fs::write(dt.join("config.json"), b"{\"dt_new\":true}").unwrap();
        fs::write(dt.join("attachments/sub/note.bin"), b"newer-bytes").unwrap();
        let preserved =
            restore_rollback_snapshot(&dt.join("snapshots").join(&snap_id), &dt).unwrap();
        assert_eq!(
            fs::read_to_string(preserved.join("config.json")).unwrap(),
            "{\"dt_new\":true}"
        );
        assert_eq!(
            fs::read(preserved.join("attachments/sub/note.bin")).unwrap(),
            b"newer-bytes"
        );
        assert_eq!(
            fs::read_to_string(dt.join("config.json")).unwrap(),
            "{\"legacy\":true}"
        );
        assert_eq!(
            fs::read(dt.join("attachments/sub/note.bin")).unwrap(),
            attachment_bytes
        );

        // Monotonic safety state must be preserved across rollback!
        let post_safety = MonotonicSafetyState::load(&dt).unwrap();
        assert!(
            post_safety
                .revoked_credential_ids
                .contains("cred-revoked-1")
        );
        assert_eq!(post_safety.spent_budget_micros, 700_000);
        assert!(post_safety.forgotten_memory_fingerprints.contains("fp-123"));
        assert!(post_safety.revoked_update_versions.contains("1.0.0-bad"));
        assert!(post_safety.paused_recording);

        // Schema downgrade check: store at v999 must fail closed.
        write_schema_marker(&dt, 999, "9.9.9").unwrap();
        assert!(migrate_user_data(None, &dt, "1.0.1", None).is_err());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_rec01_interrupted_rollback_resumes_and_enforces_limits() {
        let root = tempfile::tempdir().unwrap();
        let storage = root.path().join("dt");
        fs::create_dir_all(storage.join("attachments")).unwrap();
        fs::write(storage.join("config.json"), b"{\"v\":\"old\"}").unwrap();
        fs::write(storage.join("attachments/doc.txt"), b"original-doc").unwrap();
        let db_path = storage.join("openbot.sqlite");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE bot_tasks (id TEXT PRIMARY KEY, status TEXT, blocker_note TEXT);
             INSERT INTO bot_tasks VALUES ('job-1', 'waiting_approval', NULL);
             CREATE TABLE hosted_sessions_v1 (session_sha256 TEXT PRIMARY KEY, revoked_at TEXT);
             INSERT INTO hosted_sessions_v1 VALUES ('sess-1', NULL);",
        )
        .unwrap();
        drop(conn);

        let snap = create_rollback_snapshot(&storage, "1.0.0", &storage.join("snapshots")).unwrap();
        assert_eq!(
            list_rollback_snapshots(&storage.join("snapshots"))
                .unwrap()
                .len(),
            1
        );

        // Upgrade writes newer state.
        fs::write(storage.join("config.json"), b"{\"v\":\"new\"}").unwrap();
        fs::write(storage.join("attachments/doc.txt"), b"newer-doc").unwrap();

        // Simulate Doctor killed right after PackageSwitched (REC-01)!
        let preserved_dir = storage.join("preserved-newer-rec01");
        gm_core::storage::ensure_private_dir(&preserved_dir).unwrap();
        fs::copy(
            storage.join("config.json"),
            preserved_dir.join("config.json"),
        )
        .unwrap();
        let journal = RollbackJournal {
            schema: 1,
            run_id: "rec01-run".into(),
            snapshot_dir: snap.snapshot_dir.clone(),
            target_storage_dir: storage.clone(),
            preserved_newer_dir: preserved_dir.clone(),
            phase: RollbackPhase::PackageSwitched,
            attempts: 1,
            external_work_paused: true,
            disconnected_recovery: false,
            target_package_version: "1.0.0".into(),
            updated_at: Utc::now().to_rfc3339(),
        };
        save_rollback_journal(&storage, &journal).unwrap();

        // Restarting resumes to a compatible pair, preserves both copies, invalidates session/approval, pauses external work.
        let outcome =
            execute_journaled_rollback(&snap.snapshot_dir, &storage, "rec01-run").unwrap();
        assert_eq!(outcome.phase, RollbackPhase::Completed);
        assert!(outcome.external_work_paused);
        assert_eq!(
            fs::read_to_string(storage.join("config.json")).unwrap(),
            "{\"v\":\"old\"}"
        );
        assert_eq!(
            fs::read_to_string(preserved_dir.join("config.json")).unwrap(),
            "{\"v\":\"new\"}"
        );
        assert_eq!(
            fs::read(storage.join("attachments/doc.txt")).unwrap(),
            b"original-doc"
        );

        let conn = Connection::open(&db_path).unwrap();
        let task_status: String = conn
            .query_row("SELECT status FROM bot_tasks WHERE id='job-1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(task_status, "blocked");
        let sess_revoked: Option<String> = conn
            .query_row(
                "SELECT revoked_at FROM hosted_sessions_v1 WHERE session_sha256='sess-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(sess_revoked.is_some());

        // Corrupt backup file rejects restore.
        fs::write(snap.snapshot_dir.join("attachments/doc.txt"), b"tampered").unwrap();
        assert!(inspect_rollback_snapshot(&snap.snapshot_dir).is_err());

        // Full-disk check rejects snapshot/restore cleanly.
        assert!(check_space_available(Some(1024), 4096, "test").is_err());

        // Path traversal check rejects unsafe path.
        assert!(validate_safe_relative_path("../escape").is_err());

        // Second writer holding lease blocks restore.
        let _lease = StorageLease::acquire(&storage).unwrap();
        assert!(execute_journaled_rollback(&snap.snapshot_dir, &storage, "rec01-run-2").is_err());
    }
}
