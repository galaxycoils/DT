//! SQLite integrity checks and explicit, resumable preservation of corrupt stores.

use gm_core::database::{acquire_database_lease, corrupt_header, recovery_directory};
use gm_core::error::{GmError, Result};
use rusqlite::backup::{Backup, StepResult};
use rusqlite::{Connection, ErrorCode, OpenFlags};
use std::fs;
use std::path::Path;

const READ_ONLY: OpenFlags =
    OpenFlags::SQLITE_OPEN_READ_ONLY.union(OpenFlags::SQLITE_OPEN_NO_MUTEX);

/// Perform a non-destructive integrity check on a SQLite database.
pub fn check_sqlite_integrity(path: &Path) -> Result<()> {
    if !path.try_exists()? {
        return Ok(());
    }
    let check = quick_check(path).map_err(|e| check_error(path, e))?;
    if check.eq_ignore_ascii_case("ok") {
        Ok(())
    } else {
        Err(GmError::Platform(format!(
            "SQLite integrity check failed for {}: {check}",
            path.display()
        )))
    }
}

fn check_error(path: &Path, error: rusqlite::Error) -> GmError {
    GmError::Platform(format!(
        "SQLite integrity check failed for {}: {error}",
        path.display()
    ))
}

fn quick_check(path: &Path) -> rusqlite::Result<String> {
    let conn = Connection::open_with_flags(path, READ_ONLY)?;
    let mut check: String = conn.query_row("PRAGMA quick_check(1)", [], |row| row.get(0))?;
    if check.starts_with("unable to validate the inverted index")
        && check.ends_with("attempt to write a readonly database")
    {
        // SQLite 3.45.0 needs a writable connection to check FTS indexes.
        let mut snapshot = Connection::open("")?;
        let copied = Backup::new(&conn, &mut snapshot)?.step(-1)?;
        if !matches!(copied, StepResult::Done) {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                Some(format!(
                    "SQLite snapshot for integrity check did not complete: {copied:?}"
                )),
            ));
        }
        check = snapshot.query_row("PRAGMA quick_check(1)", [], |row| row.get(0))?;
    }
    if check.starts_with("unable to validate the inverted index") {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
            Some(check),
        ));
    }
    Ok(check)
}

/// Preserve the database and its sidecars by moving them into a private
/// quarantine on the same filesystem. A subsequent normal open initializes it.
pub fn quarantine_corrupt_database(path: &Path) -> Result<bool> {
    let staging = recovery_directory(path);
    if !path.try_exists()? && !staging.try_exists()? {
        return Ok(false);
    }
    let _lease = acquire_database_lease(path, true)?;
    if !staging.try_exists()? {
        if !corrupt_header(path)? {
            match quick_check(path) {
                Ok(check) if check.eq_ignore_ascii_case("ok") => return Ok(false),
                Ok(_) => {}
                Err(error)
                    if matches!(
                        error.sqlite_error_code(),
                        Some(ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase)
                    ) => {}
                Err(error) => return Err(check_error(path, error)),
            }
        }
        gm_core::storage::ensure_private_dir(&staging)?;
    }
    if !staging.symlink_metadata()?.file_type().is_dir() {
        return Err(GmError::SafetyViolation(
            "Invalid database recovery directory".into(),
        ));
    }
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        let source = Path::new(&name);
        let destination = staging.join(
            source
                .file_name()
                .ok_or_else(|| GmError::Config("Missing database file name".into()))?,
        );
        match source.symlink_metadata() {
            Ok(metadata) if metadata.is_file() && !destination.try_exists()? => {
                fs::rename(source, destination)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
            Ok(_) => {
                return Err(GmError::SafetyViolation(format!(
                    "Unexpected database recovery entry {}",
                    source.display()
                )));
            }
        }
    }
    let quarantine = path.with_extension(format!("corrupt-{}.bak", uuid::Uuid::new_v4()));
    fs::rename(staging, quarantine)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gm_core::database::Database;

    #[test]
    fn database_recovery_is_explicit_and_resumable() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("test.sqlite");
        let original = b"NOT_A_VALID_SQLITE_DATABASE";
        fs::write(&path, original).unwrap();
        assert!(Database::open(&path).is_err());
        assert_eq!(fs::read(&path).unwrap(), original);

        let staged = recovery_directory(&path);
        fs::create_dir(&staged).unwrap();
        fs::rename(&path, staged.join("test.sqlite")).unwrap();
        fs::write(root.path().join("test.sqlite-wal"), b"wal").unwrap();
        assert!(Database::open(&path).is_err());
        assert!(quarantine_corrupt_database(&path).unwrap());
        assert!(!path.exists());

        let backup = fs::read_dir(root.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|name| name.extension().is_some_and(|ext| ext == "bak"))
            .unwrap();
        assert_eq!(fs::read(backup.join("test.sqlite")).unwrap(), original);
        assert_eq!(fs::read(backup.join("test.sqlite-wal")).unwrap(), b"wal");

        let live = Database::open(&path).unwrap();
        live.execute_batch(
            "CREATE TABLE notes (body TEXT); INSERT INTO notes VALUES ('preserved');",
        )
        .unwrap();
        assert!(quarantine_corrupt_database(&path).is_err());
        drop(live);
        assert!(!quarantine_corrupt_database(&path).unwrap());
        let live = Database::open(&path).unwrap();
        assert_eq!(
            live.query_row("SELECT body FROM notes", [], |row| row.get::<_, String>(0))
                .unwrap(),
            "preserved"
        );
    }
}
