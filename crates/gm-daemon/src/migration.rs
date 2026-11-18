//! Versioned data migration, consistent SQLite snapshotting, dual-store conflict preservation,
//! and coordinated package+data rollback for DT v1.0.0.

use chrono::Utc;
use gm_core::error::{GmError, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

pub const CURRENT_DATA_SCHEMA_VERSION: u32 = 2;

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

fn read_schema_marker(storage_dir: &Path) -> Result<Option<u32>> {
    let marker = storage_dir.join("dt-schema-version.json");
    if !marker.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&marker)?;
    let val: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| GmError::Config(format!("Corrupt schema marker {}: {e}", marker.display())))?;
    let ver = val["schema_version"]
        .as_u64()
        .ok_or_else(|| GmError::Config("Missing schema_version in marker".into()))?;
    Ok(Some(ver as u32))
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

/// Snapshot a SQLite database consistently using SQLite `VACUUM INTO`.
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
        !name.starts_with('.') && name != "dt-schema-version.json"
    })
}

/// Snapshot all critical stores under an exclusive storage lease before upgrade or rollback.
pub fn create_rollback_snapshot(
    storage_dir: &Path,
    package_version: &str,
    snapshots_root: &Path,
) -> Result<RollbackSnapshot> {
    let _lease = StorageLease::acquire(storage_dir)?;
    let schema_ver = read_schema_marker(storage_dir)?.unwrap_or(1);
    let snapshot_id = format!(
        "snap-{}-{}",
        Utc::now().format("%Y%m%dT%H%M%SZ"),
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    let snapshot_dir = snapshots_root.join(&snapshot_id);
    gm_core::storage::ensure_private_dir(&snapshot_dir)?;

    let mut files = Vec::new();
    let known_stores = [
        "config.json",
        "openbot.sqlite",
        "channels.sqlite",
        "usage.sqlite3",
        "memory.sqlite",
        "audit.jsonl",
        "conversations.json",
        "drafts.json",
        "tasks.json",
        "credentials.ref.json",
    ];

    for rel in known_stores {
        let src = storage_dir.join(rel);
        if !src.is_file() {
            continue;
        }
        let dst = snapshot_dir.join(rel);
        if rel.ends_with(".sqlite") || rel.ends_with(".sqlite3") {
            backup_sqlite_consistent(&src, &dst)?;
        } else {
            fs::copy(&src, &dst)?;
        }
        let (sha256, size_bytes) = sha256_file(&dst)?;
        files.push(SnapshotFileRecord {
            relative_path: rel.to_string(),
            sha256,
            size_bytes,
        });
    }

    let snapshot = RollbackSnapshot {
        schema: 1,
        snapshot_id,
        package_version: package_version.to_string(),
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

/// Migrate from v0.7.0/legacy `grok-muse` stores into `dt` while preserving custom paths,
/// preventing schema downgrades, and halting safely if both legacy and DT stores contain data.
pub fn migrate_user_data(
    legacy_dir: Option<&Path>,
    dt_storage_dir: &Path,
    package_version: &str,
    choice: Option<DualStoreChoice>,
) -> Result<MigrationOutcome> {
    if let Some(existing) = read_schema_marker(dt_storage_dir)? {
        if existing > CURRENT_DATA_SCHEMA_VERSION {
            return Err(GmError::Config(format!(
                "Schema downgrade refused: store schema v{existing} is newer than DT binary schema v{CURRENT_DATA_SCHEMA_VERSION}"
            )));
        }
        if existing == CURRENT_DATA_SCHEMA_VERSION {
            return Ok(MigrationOutcome {
                status: "up_to_date".into(),
                from_schema: existing,
                to_schema: CURRENT_DATA_SCHEMA_VERSION,
                snapshot_id: None,
                dual_store_conflict: false,
                message: "User data schema is already current".into(),
            });
        }
    }

    let legacy_has_data = legacy_dir.is_some_and(dir_has_user_data);
    let dt_has_data = dir_has_user_data(dt_storage_dir);

    if legacy_has_data && dt_has_data && choice.is_none() {
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

    gm_core::storage::ensure_private_dir(dt_storage_dir)?;
    let _lease = StorageLease::acquire(dt_storage_dir)?;
    let snapshots_root = dt_storage_dir.join("snapshots");
    let source_for_snapshot = match (legacy_dir, legacy_has_data, dt_has_data, choice) {
        (Some(legacy), true, false, _)
        | (Some(legacy), true, true, Some(DualStoreChoice::PreserveLegacy)) => legacy,
        _ => dt_storage_dir,
    };

    let from_schema = read_schema_marker(source_for_snapshot)?.unwrap_or(1);
    let snapshot = if dir_has_user_data(source_for_snapshot) {
        let snap =
            create_rollback_snapshot_unlocked(source_for_snapshot, "0.7.0", &snapshots_root)?;
        if source_for_snapshot != dt_storage_dir {
            let staging = dt_storage_dir.join(format!(".migration-stage-{}", snap.snapshot_id));
            gm_core::storage::ensure_private_dir(&staging)?;
            for file in &snap.files {
                let src = snap.snapshot_dir.join(&file.relative_path);
                let dst = staging.join(&file.relative_path);
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
                let staged = staging.join(&file.relative_path);
                let target = dt_storage_dir.join(&file.relative_path);
                if target.exists() {
                    let preserved = dt_storage_dir.join(format!(
                        "{}.pre-migration.{}.bak",
                        file.relative_path, snap.snapshot_id
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

fn create_rollback_snapshot_unlocked(
    storage_dir: &Path,
    package_version: &str,
    snapshots_root: &Path,
) -> Result<RollbackSnapshot> {
    let schema_ver = read_schema_marker(storage_dir)?.unwrap_or(1);
    let snapshot_id = format!(
        "snap-{}-{}",
        Utc::now().format("%Y%m%dT%H%M%SZ"),
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    let snapshot_dir = snapshots_root.join(&snapshot_id);
    gm_core::storage::ensure_private_dir(&snapshot_dir)?;

    let mut files = Vec::new();
    if let Ok(entries) = fs::read_dir(storage_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if name.starts_with('.') || name == "dt-schema-version.json" {
                continue;
            }
            let dst = snapshot_dir.join(name);
            if name.ends_with(".sqlite") || name.ends_with(".sqlite3") {
                backup_sqlite_consistent(&path, &dst)?;
            } else {
                fs::copy(&path, &dst)?;
            }
            let (sha256, size_bytes) = sha256_file(&dst)?;
            files.push(SnapshotFileRecord {
                relative_path: name.to_string(),
                sha256,
                size_bytes,
            });
        }
    }

    let snapshot = RollbackSnapshot {
        schema: 1,
        snapshot_id,
        package_version: package_version.to_string(),
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

/// Restore a verified rollback snapshot while preserving newer post-upgrade data separately.
pub fn restore_rollback_snapshot(
    snapshot_dir: &Path,
    target_storage_dir: &Path,
) -> Result<PathBuf> {
    let manifest_raw = fs::read_to_string(snapshot_dir.join("rollback.json"))?;
    let snapshot: RollbackSnapshot = serde_json::from_str(&manifest_raw)?;
    for file in &snapshot.files {
        let src = snapshot_dir.join(&file.relative_path);
        let (actual_sha, _) = sha256_file(&src)?;
        if actual_sha != file.sha256 {
            return Err(GmError::SafetyViolation(format!(
                "Rollback snapshot file {} failed SHA-256 verification",
                file.relative_path
            )));
        }
    }

    let _lease = StorageLease::acquire(target_storage_dir)?;
    let preserved_newer_dir = target_storage_dir.join(format!(
        "preserved-newer-{}",
        Utc::now().format("%Y%m%dT%H%M%SZ")
    ));
    gm_core::storage::ensure_private_dir(&preserved_newer_dir)?;
    if let Ok(entries) = fs::read_dir(target_storage_dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if !p.is_file() {
                continue;
            }
            if let Some(name) = p.file_name().and_then(|n| n.to_str())
                && !name.starts_with('.')
            {
                fs::copy(&p, preserved_newer_dir.join(name))?;
            }
        }
    }

    for file in &snapshot.files {
        let src = snapshot_dir.join(&file.relative_path);
        let dst = target_storage_dir.join(&file.relative_path);
        fs::copy(&src, &dst)?;
    }
    write_schema_marker(
        target_storage_dir,
        snapshot.data_schema_version,
        &snapshot.package_version,
    )?;
    Ok(preserved_newer_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_migration_dual_store_conflict_and_rollback() {
        let root = std::env::temp_dir().join(format!("dt-mig-test-{}", uuid::Uuid::new_v4()));
        let legacy = root.join("grok-muse");
        let dt = root.join("dt");
        fs::create_dir_all(&legacy).unwrap();
        fs::create_dir_all(&dt).unwrap();

        // Populate legacy SQLite + config and DT store simultaneously -> must detect conflict and preserve both.
        let legacy_db = legacy.join("openbot.sqlite");
        let conn = Connection::open(&legacy_db).unwrap();
        conn.execute_batch("CREATE TABLE t (v TEXT); INSERT INTO t VALUES ('legacy');")
            .unwrap();
        drop(conn);
        fs::write(legacy.join("config.json"), b"{\"legacy\":true}").unwrap();
        fs::write(dt.join("config.json"), b"{\"dt\":true}").unwrap();

        let conflict = migrate_user_data(Some(&legacy), &dt, "1.0.0", None).unwrap();
        assert!(conflict.dual_store_conflict);
        assert_eq!(conflict.status, "conflict_requires_choice");
        assert!(legacy_db.exists());
        assert!(dt.join("config.json").exists());

        // Choosing PreserveLegacy stages & verifies snapshot, then sets schema v2.
        let migrated = migrate_user_data(
            Some(&legacy),
            &dt,
            "1.0.0",
            Some(DualStoreChoice::PreserveLegacy),
        )
        .unwrap();
        assert_eq!(migrated.status, "migrated");
        let snap_id = migrated.snapshot_id.unwrap();

        // Simulate newer data written after upgrade, then rollback.
        fs::write(dt.join("config.json"), b"{\"dt_new\":true}").unwrap();
        let preserved =
            restore_rollback_snapshot(&dt.join("snapshots").join(&snap_id), &dt).unwrap();
        assert_eq!(
            fs::read_to_string(preserved.join("config.json")).unwrap(),
            "{\"dt_new\":true}"
        );
        assert_eq!(
            fs::read_to_string(dt.join("config.json")).unwrap(),
            "{\"legacy\":true}"
        );

        // Schema downgrade check: store at v999 must fail closed.
        write_schema_marker(&dt, 999, "9.9.9").unwrap();
        assert!(migrate_user_data(None, &dt, "1.0.0", None).is_err());

        let _ = fs::remove_dir_all(&root);
    }
}
