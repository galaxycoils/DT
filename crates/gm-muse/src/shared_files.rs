//! Concurrency-safe shared runtime file store combining per-path async
//! mutexes, cross-process directory advisory locks, and SHA-256 optimistic CAS.

use gm_core::error::{GmError, Result};
use gm_core::storage::{atomic_write_private, ensure_private_dir};
use hex::encode as hex_encode;
use parking_lot::Mutex as SyncMutex;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use tokio::sync::Mutex as AsyncMutex;

/// Result of a CAS/serialized update through [`SharedFileStore`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SharedUpdateOutcome {
    Updated { sha256: String, bytes: usize },
    Conflict { current_sha256: Option<String> },
}

#[derive(Default)]
struct LockRegistry {
    async_locks: SyncMutex<HashMap<PathBuf, Arc<AsyncMutex<()>>>>,
    blocking_locks: SyncMutex<HashMap<PathBuf, Arc<SyncMutex<()>>>>,
}

impl LockRegistry {
    fn async_mutex_for(&self, key: &Path) -> Arc<AsyncMutex<()>> {
        let mut guard = self.async_locks.lock();
        guard
            .entry(key.to_path_buf())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    }

    fn blocking_mutex_for(&self, key: &Path) -> Arc<SyncMutex<()>> {
        let mut guard = self.blocking_locks.lock();
        guard
            .entry(key.to_path_buf())
            .or_insert_with(|| Arc::new(SyncMutex::new(())))
            .clone()
    }
}

fn global_registry() -> &'static LockRegistry {
    static REGISTRY: OnceLock<LockRegistry> = OnceLock::new();
    REGISTRY.get_or_init(LockRegistry::default)
}

#[cfg(unix)]
const fn unix_o_nofollow() -> i32 {
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "dragonfly"
    ))]
    {
        0x0100
    }
    #[cfg(all(
        target_os = "linux",
        any(
            target_arch = "aarch64",
            target_arch = "arm",
            target_arch = "powerpc",
            target_arch = "powerpc64"
        )
    ))]
    {
        0o100_000
    }
    #[cfg(all(
        target_os = "linux",
        not(any(
            target_arch = "aarch64",
            target_arch = "arm",
            target_arch = "powerpc",
            target_arch = "powerpc64"
        ))
    ))]
    {
        0o400_000
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "dragonfly"
    )))]
    {
        0
    }
}

/// Normalize a path key without following the leaf (which might not exist yet)
/// so equivalent relative and canonical paths serialize on the same lock entry.
fn normalize_lock_key(path: &Path) -> PathBuf {
    if let Ok(canon) = std::fs::canonicalize(path) {
        return canon;
    }
    if let Some(parent) = path.parent()
        && let Some(name) = path.file_name()
        && let Ok(canon_parent) = std::fs::canonicalize(parent)
    {
        return canon_parent.join(name);
    }
    if path.is_absolute() {
        path.to_path_buf()
    } else if let Ok(cwd) = std::env::current_dir() {
        cwd.join(path)
    } else {
        path.to_path_buf()
    }
}

fn ensure_not_symlink(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(GmError::SafetyViolation(format!(
                    "RULE-PATH-002: Refused symlink target '{}'",
                    path.display()
                )));
            }
            Ok(true)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(GmError::Io(err)),
    }
}

fn lock_directory_no_follow(dir: &Path) -> Result<std::fs::File> {
    ensure_private_dir(dir)?;
    let lock_path = dir.join(".muse.lock");
    let _ = ensure_not_symlink(&lock_path)?;

    let mut options = std::fs::OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(unix_o_nofollow());
    }
    let file = options.open(&lock_path).map_err(|err| {
        if err.raw_os_error() == Some(40) || err.raw_os_error() == Some(62) {
            GmError::SafetyViolation(format!(
                "RULE-PATH-002: Refused symlink lock file '{}'",
                lock_path.display()
            ))
        } else {
            GmError::Io(err)
        }
    })?;
    let _ = ensure_not_symlink(&lock_path)?;
    file.lock()?;
    Ok(file)
}

/// Process-wide and cross-process concurrency controller for shared Muse markdown and YAML files.
#[derive(Debug, Clone, Default)]
pub struct SharedFileStore;

impl SharedFileStore {
    pub fn new() -> Self {
        Self
    }

    pub fn global() -> &'static Self {
        static GLOBAL: OnceLock<SharedFileStore> = OnceLock::new();
        GLOBAL.get_or_init(SharedFileStore::new)
    }

    pub fn sha256_hex(bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        hex_encode(hasher.finalize())
    }

    /// Read a UTF-8 file and its SHA-256 hex digest under directory lock protection,
    /// returning `Ok(None)` if the file does not exist and rejecting symlinks with `RULE-PATH-002`.
    pub fn read_text_with_sha256(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<Option<(String, String)>> {
        let path = path.as_ref();
        let key = normalize_lock_key(path);
        let blocking = global_registry().blocking_mutex_for(&key);
        let _blocking_guard = blocking.lock();

        if !ensure_not_symlink(path)? {
            return Ok(None);
        }
        let meta = std::fs::symlink_metadata(path)?;
        if !meta.is_file() {
            return Err(GmError::SafetyViolation(format!(
                "RULE-PATH-002: Expected regular file at '{}'",
                path.display()
            )));
        }
        let bytes = std::fs::read(path)?;
        let sha256 = Self::sha256_hex(&bytes);
        let text = String::from_utf8(bytes).map_err(|_| {
            GmError::Config(format!("File '{}' is not valid UTF-8", path.display()))
        })?;
        Ok(Some((text, sha256)))
    }

    /// Update a shared UTF-8 file with per-path async serialization, cross-process `.muse.lock`,
    /// optional optimistic SHA-256 compare-and-swap, and atomic `0600` replacement.
    pub async fn update<F>(
        &self,
        path: impl AsRef<Path>,
        expected_sha256: Option<&str>,
        f: F,
    ) -> Result<SharedUpdateOutcome>
    where
        F: FnOnce(&str) -> Result<String> + Send + 'static,
    {
        let path_buf = path.as_ref().to_path_buf();
        let expected = expected_sha256.map(str::to_string);
        let key = normalize_lock_key(&path_buf);
        let async_lock = global_registry().async_mutex_for(&key);
        let _async_guard = async_lock.lock().await;

        tokio::task::spawn_blocking(move || {
            Self::update_locked_inner(&path_buf, expected.as_deref(), f)
        })
        .await
        .map_err(|e| GmError::Platform(format!("SharedFileStore task join error: {e}")))?
    }

    /// Synchronous equivalent of [`SharedFileStore::update`] for blocking/SQLite workflows.
    pub fn update_sync<F>(
        &self,
        path: impl AsRef<Path>,
        expected_sha256: Option<&str>,
        f: F,
    ) -> Result<SharedUpdateOutcome>
    where
        F: FnOnce(&str) -> Result<String>,
    {
        Self::update_locked_inner(path.as_ref(), expected_sha256, f)
    }

    /// Replace a shared UTF-8 file with `content`, honoring optional `expected_sha256` CAS.
    pub fn write_exact_sync(
        &self,
        path: impl AsRef<Path>,
        expected_sha256: Option<&str>,
        content: &str,
    ) -> Result<SharedUpdateOutcome> {
        let next = content.to_string();
        self.update_sync(path, expected_sha256, move |_| Ok(next))
    }

    /// Atomically delete an exact regular file (never directories or symlinks) under directory lock.
    pub fn remove_file_sync(
        &self,
        path: impl AsRef<Path>,
        expected_sha256: Option<&str>,
    ) -> Result<bool> {
        let path = path.as_ref();
        let parent = path
            .parent()
            .ok_or_else(|| GmError::Config("Target file has no parent directory".into()))?;
        let key = normalize_lock_key(path);
        let blocking = global_registry().blocking_mutex_for(&key);
        let _blocking_guard = blocking.lock();

        if !parent.exists() {
            return Ok(false);
        }
        let _dir_lock = lock_directory_no_follow(parent)?;
        if !ensure_not_symlink(path)? {
            return Ok(false);
        }
        let meta = std::fs::symlink_metadata(path)?;
        if !meta.is_file() {
            return Err(GmError::SafetyViolation(format!(
                "RULE-PATH-002: Refused non-file removal target '{}'",
                path.display()
            )));
        }
        if let Some(expected) = expected_sha256 {
            let bytes = std::fs::read(path)?;
            let current = Self::sha256_hex(&bytes);
            if current != expected {
                return Err(GmError::Config(format!(
                    "File '{}' changed concurrently (expected sha256={}, found={})",
                    path.display(),
                    expected,
                    current
                )));
            }
        }
        std::fs::remove_file(path)?;
        Ok(true)
    }

    fn update_locked_inner<F>(
        path: &Path,
        expected_sha256: Option<&str>,
        f: F,
    ) -> Result<SharedUpdateOutcome>
    where
        F: FnOnce(&str) -> Result<String>,
    {
        let parent = path
            .parent()
            .ok_or_else(|| GmError::Config("Shared file path has no parent directory".into()))?;
        let key = normalize_lock_key(path);
        let blocking = global_registry().blocking_mutex_for(&key);
        let _blocking_guard = blocking.lock();
        let _dir_lock = lock_directory_no_follow(parent)?;

        let exists = ensure_not_symlink(path)?;
        let (current_text, current_sha256) = if exists {
            let meta = std::fs::symlink_metadata(path)?;
            if !meta.is_file() {
                return Err(GmError::SafetyViolation(format!(
                    "RULE-PATH-002: Target '{}' is not a regular file",
                    path.display()
                )));
            }
            let bytes = std::fs::read(path)?;
            let sha = Self::sha256_hex(&bytes);
            let text = String::from_utf8(bytes).map_err(|_| {
                GmError::Config(format!("File '{}' is not valid UTF-8", path.display()))
            })?;
            (text, Some(sha))
        } else {
            (String::new(), None)
        };

        if let Some(expected) = expected_sha256
            && current_sha256.as_deref() != Some(expected)
        {
            return Ok(SharedUpdateOutcome::Conflict { current_sha256 });
        }

        let updated = f(&current_text)?;
        let bytes = updated.as_bytes();
        atomic_write_private(path, bytes)?;
        let sha256 = Self::sha256_hex(bytes);
        Ok(SharedUpdateOutcome::Updated {
            sha256,
            bytes: bytes.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn five_tasks_doing_50_concurrent_appends_produce_50_lines() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("MEMORY.md");
        let store = SharedFileStore::new();

        let mut handles = Vec::new();
        for task_id in 0..5 {
            let store_cloned = store.clone();
            let path_cloned = target.clone();
            handles.push(tokio::spawn(async move {
                for seq in 0..10 {
                    let outcome = store_cloned
                        .update(&path_cloned, None, move |existing| {
                            Ok(format!("{existing}task-{task_id}-line-{seq}\n"))
                        })
                        .await
                        .unwrap();
                    assert!(matches!(outcome, SharedUpdateOutcome::Updated { .. }));
                }
            }));
        }
        for handle in handles {
            handle.await.unwrap();
        }

        let content = std::fs::read_to_string(&target).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 50, "Expected 50 unique appended lines");
        for task_id in 0..5 {
            for seq in 0..10 {
                let needle = format!("task-{task_id}-line-{seq}");
                assert!(lines.contains(&needle.as_str()), "Missing {needle}");
            }
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[tokio::test]
    async fn optimistic_cas_detects_conflict_and_returns_current_sha256() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("USER.md");
        let store = SharedFileStore::new();

        let initial = store
            .update(&target, None, |_| Ok("# User\ninitial\n".into()))
            .await
            .unwrap();
        let SharedUpdateOutcome::Updated { sha256: sha1, .. } = initial else {
            panic!("expected initial update");
        };

        let stale = store
            .update(&target, Some("00000000"), |_| Ok("stale overwrite".into()))
            .await
            .unwrap();
        assert_eq!(
            stale,
            SharedUpdateOutcome::Conflict {
                current_sha256: Some(sha1.clone()),
            }
        );
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "# User\ninitial\n"
        );

        let next = store
            .update(&target, Some(&sha1), |old| Ok(format!("{old}updated\n")))
            .await
            .unwrap();
        assert!(matches!(next, SharedUpdateOutcome::Updated { .. }));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn refuses_symlink_target_and_symlink_lock_file() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside.txt");
        std::fs::write(&outside, "secret").unwrap();
        let link = dir.path().join("MEMORY.md");
        std::os::unix::fs::symlink(&outside, &link).unwrap();

        let store = SharedFileStore::new();
        let err = store
            .update(&link, None, |_| Ok("overwritten".into()))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("RULE-PATH-002"));
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "secret");

        let lock_dir = dir.path().join("locked_sub");
        std::fs::create_dir_all(&lock_dir).unwrap();
        std::os::unix::fs::symlink(&outside, lock_dir.join(".muse.lock")).unwrap();
        let err_lock = store
            .update(lock_dir.join("file.md"), None, |_| Ok("ok".into()))
            .await
            .unwrap_err();
        assert!(err_lock.to_string().contains("RULE-PATH-002"));
    }
}
