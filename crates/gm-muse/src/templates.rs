//! Embedded upstream Muse assets, Grok-Muse adaptation templates, runtime
//! workspace bootstrapping, and idempotent schema migrations.

use gm_core::error::{GmError, Result};
use gm_core::muse_paths::MuseRuntimePaths;
use gm_core::storage::{atomic_write_private, ensure_private_dir};
use include_dir::{Dir, include_dir};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Current Muse runtime storage schema version recorded in `<storage>/.muse_schema_version`.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// Bundled upstream and Grok-Muse assets embedded verbatim into the binary.
pub static ASSETS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/assets");

static REFRESH_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Per-bot identity seed passed to [`bootstrap_bot_runtime_files`].
#[derive(Debug, Clone)]
pub struct BotSeed<'a> {
    pub id: &'a str,
    pub display_name: &'a str,
    pub role_title: &'a str,
    pub legacy_system_prompt: &'a str,
}

/// Summary of filesystem actions taken during runtime bootstrapping.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BootstrapReport {
    pub created: Vec<PathBuf>,
    pub refreshed: Vec<PathBuf>,
    pub preserved: Vec<PathBuf>,
}

/// Read an embedded UTF-8 text asset relative to `crates/gm-muse/assets/`.
pub fn read_asset_str(rel_path: &str) -> Result<&'static str> {
    let file = ASSETS
        .get_file(rel_path)
        .ok_or_else(|| GmError::Config(format!("Missing embedded Muse asset '{rel_path}'")))?;
    file.contents_utf8()
        .ok_or_else(|| GmError::Config(format!("Embedded Muse asset '{rel_path}' is not UTF-8")))
}

/// Read an embedded raw byte asset relative to `crates/gm-muse/assets/`.
pub fn read_asset_bytes(rel_path: &str) -> Result<&'static [u8]> {
    let file = ASSETS
        .get_file(rel_path)
        .ok_or_else(|| GmError::Config(format!("Missing embedded Muse asset '{rel_path}'")))?;
    Ok(file.contents())
}

fn path_exists_no_follow(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

fn create_if_absent(dest: &Path, bytes: &[u8], report: &mut BootstrapReport) -> Result<()> {
    if path_exists_no_follow(dest) {
        report.preserved.push(dest.to_path_buf());
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        ensure_private_dir(parent)?;
    }
    atomic_write_private(dest, bytes)?;
    report.created.push(dest.to_path_buf());
    Ok(())
}

fn copy_dir_tree_private(
    embedded_dir: &Dir<'_>,
    base_prefix: &str,
    dest_dir: &Path,
) -> Result<Vec<PathBuf>> {
    ensure_private_dir(dest_dir)?;
    let mut written = Vec::new();
    for file in embedded_dir.files() {
        let rel = file.path().strip_prefix(base_prefix).unwrap_or(file.path());
        let target = dest_dir.join(rel);
        if let Some(parent) = target.parent() {
            ensure_dir_tree_private(dest_dir, parent)?;
        }
        atomic_write_private(&target, file.contents())?;
        written.push(target);
    }
    for sub in embedded_dir.dirs() {
        let rel = sub.path().strip_prefix(base_prefix).unwrap_or(sub.path());
        let sub_dest = dest_dir.join(rel);
        ensure_dir_tree_private(dest_dir, &sub_dest)?;
        let mut inner = copy_dir_tree_private(sub, base_prefix, dest_dir)?;
        written.append(&mut inner);
    }
    Ok(written)
}

fn ensure_dir_tree_private(root: &Path, target: &Path) -> Result<()> {
    if let Ok(rel) = target.strip_prefix(root) {
        let mut cursor = root.to_path_buf();
        ensure_private_dir(&cursor)?;
        for comp in rel.components() {
            cursor.push(comp);
            ensure_private_dir(&cursor)?;
        }
    } else {
        ensure_private_dir(target)?;
    }
    Ok(())
}

/// Atomically replace a read-only asset directory (`skills/` or `docs/`) by writing
/// into a private staging directory and swapping via rename.
fn refresh_readonly_dir_tree(
    storage: &Path,
    asset_subdir: &str,
    final_dir: &Path,
    report: &mut BootstrapReport,
) -> Result<()> {
    let embedded = ASSETS.get_dir(asset_subdir).ok_or_else(|| {
        GmError::Config(format!("Missing embedded asset directory '{asset_subdir}'"))
    })?;
    let seq = REFRESH_COUNTER.fetch_add(1, Ordering::Relaxed);
    let staging = storage.join(format!(
        ".muse-refresh-{}-{}-{seq}",
        final_dir
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("assets"),
        std::process::id(),
    ));
    if path_exists_no_follow(&staging) {
        let _ = std::fs::remove_dir_all(&staging);
    }
    ensure_private_dir(&staging)?;

    let write_res = copy_dir_tree_private(embedded, asset_subdir, &staging);
    if let Err(err) = write_res {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(err);
    }

    let backup = storage.join(format!(
        ".muse-backup-{}-{}-{seq}",
        final_dir
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("assets"),
        std::process::id(),
    ));
    if path_exists_no_follow(&backup) {
        let _ = std::fs::remove_dir_all(&backup);
    }

    let had_existing = path_exists_no_follow(final_dir);
    if had_existing {
        std::fs::rename(final_dir, &backup)?;
    }
    if let Err(err) = std::fs::rename(&staging, final_dir) {
        if had_existing {
            let _ = std::fs::rename(&backup, final_dir);
        }
        let _ = std::fs::remove_dir_all(&staging);
        return Err(err.into());
    }
    if had_existing {
        let _ = std::fs::remove_dir_all(&backup);
    }

    report.refreshed.push(final_dir.to_path_buf());
    Ok(())
}

/// Materialize shared user-level runtime files (create-if-absent for user data;
/// always refresh read-only bundled `skills/` and `docs/`).
pub fn bootstrap_shared_runtime_files(paths: &MuseRuntimePaths) -> Result<BootstrapReport> {
    let mut report = BootstrapReport::default();

    for dir in [
        paths.storage_dir().to_path_buf(),
        paths.memory_dir(),
        paths.memory_dir().join("bank"),
        paths.people_dir(),
        paths.groups_dir(),
        paths.memory_dir().join("shopping"),
        paths.memory_dir().join("travel"),
        paths.dreams_dir(),
        paths.dreams_daily_dir(),
        paths.dreams_dir().join("alignment/derived"),
        paths.goals_dir(),
        paths.side_chats_dir(),
        paths.custom_skills_dir(),
        paths.custom_connectors_dir(),
        paths.connectors_dir(),
        paths.spaces_dir(),
        paths.self_improvement_migrations_dir(),
        paths.workspace_goals_dir(),
        paths.skill_amendments_dir(),
        paths.connector_amendments_dir(),
        paths.storage_dir().join("workspace/user"),
        paths.storage_dir().join("workspace/system"),
        paths.storage_dir().join("workspace/memory/forget"),
    ] {
        ensure_private_dir(&dir)?;
    }

    // Create-if-absent user and memory templates
    create_if_absent(
        &paths.user_md(),
        read_asset_bytes("upstream/USER.md")?,
        &mut report,
    )?;
    create_if_absent(
        &paths.memory_md(),
        read_asset_bytes("upstream/MEMORY.md")?,
        &mut report,
    )?;
    create_if_absent(
        &paths.proactive_preferences_md(),
        read_asset_bytes("upstream/PROACTIVE_PREFERENCES.md")?,
        &mut report,
    )?;
    create_if_absent(
        &paths.feedback_md(),
        b"# FEEDBACK.md\n\n_Optional style notes, format preferences, and explicit corrections captured during use._\n",
        &mut report,
    )?;
    create_if_absent(
        &paths.bank_experience_md(),
        read_asset_bytes("upstream/memory/bank/experience.md")?,
        &mut report,
    )?;
    create_if_absent(
        &paths.bank_opinions_md(),
        read_asset_bytes("upstream/memory/bank/opinions.md")?,
        &mut report,
    )?;
    create_if_absent(
        &paths.bank_world_md(),
        read_asset_bytes("upstream/memory/bank/world.md")?,
        &mut report,
    )?;
    create_if_absent(
        &paths.people_index_md(),
        read_asset_bytes("grok_muse/people_index.md")?,
        &mut report,
    )?;
    create_if_absent(
        &paths.groups_index_md(),
        read_asset_bytes("grok_muse/groups_index.md")?,
        &mut report,
    )?;
    create_if_absent(
        &paths.alignment_synthesis_md(),
        read_asset_bytes("grok_muse/ALIGNMENT_SYNTHESIS.md")?,
        &mut report,
    )?;

    // Always refresh bundled read-only skills/ and docs/
    refresh_readonly_dir_tree(
        paths.storage_dir(),
        "upstream/skills",
        &paths.skills_dir(),
        &mut report,
    )?;
    refresh_readonly_dir_tree(
        paths.storage_dir(),
        "upstream/docs",
        &paths.docs_dir(),
        &mut report,
    )?;

    Ok(report)
}

fn format_role_section(seed: &BotSeed<'_>) -> String {
    let role_asset = format!("grok_muse/roles/{}.md", seed.id);
    if let Ok(built_in_role) = read_asset_str(&role_asset) {
        format!("## Role in this team\n\n{}\n", built_in_role.trim())
    } else {
        format!(
            "## Role in this team\n\n- **Bot ID:** `{}`\n- **Display Name:** {}\n- **Role:** {}\n",
            seed.id, seed.display_name, seed.role_title
        )
    }
}

/// Replaces or removes the managed `## Legacy instructions` section (and updates
/// non-built-in `## Role in this team` metadata) while preserving all authored
/// `IDENTITY.md` content.
pub fn sync_identity_markdown(existing: &str, seed: &BotSeed<'_>) -> String {
    let mut kept_blocks: Vec<String> = Vec::new();
    let mut current_heading: Option<String> = None;
    let mut current_lines: Vec<&str> = Vec::new();

    let flush_block = |heading: &Option<String>, lines: &mut Vec<&str>, out: &mut Vec<String>| {
        if heading.is_none() && lines.is_empty() {
            return;
        }
        let h_trim = heading.as_deref().unwrap_or("").trim();
        if h_trim.eq_ignore_ascii_case("## Legacy instructions") {
            lines.clear();
            return;
        }
        if h_trim.eq_ignore_ascii_case("## Role in this team") {
            let role_asset = format!("grok_muse/roles/{}.md", seed.id);
            if read_asset_str(&role_asset).is_err() {
                out.push(format_role_section(seed).trim_end().to_string());
                lines.clear();
                return;
            }
        }
        let mut block = String::new();
        if let Some(h) = heading {
            block.push_str(h);
            block.push('\n');
        }
        if !lines.is_empty() {
            block.push_str(&lines.join("\n"));
            block.push('\n');
        }
        out.push(block.trim_end().to_string());
        lines.clear();
    };

    for line in existing.lines() {
        if line.trim_start().starts_with("## ") {
            flush_block(&current_heading, &mut current_lines, &mut kept_blocks);
            current_heading = Some(line.to_string());
        } else {
            current_lines.push(line);
        }
    }
    flush_block(&current_heading, &mut current_lines, &mut kept_blocks);

    let mut result = kept_blocks.join("\n\n");
    if !result.ends_with('\n') {
        result.push('\n');
    }
    let legacy = seed.legacy_system_prompt.trim();
    if !legacy.is_empty() {
        result.push_str("\n## Legacy instructions\n\n");
        result.push_str(legacy);
        result.push('\n');
    }
    result
}

/// Materialize per-bot persona files (`SOUL.md`, `IDENTITY.md`, `AGENTS.md`,
/// `TOOLS.md`, `HEARTBEAT.md`) and isolate workspace/inbox/outbox dirs.
pub fn bootstrap_bot_runtime_files(
    paths: &MuseRuntimePaths,
    seed: &BotSeed<'_>,
) -> Result<BootstrapReport> {
    let mut report = BootstrapReport::default();
    let bot_root = paths.bot_root.as_ref().ok_or_else(|| {
        GmError::Config(format!(
            "Cannot bootstrap bot '{}' without an active bot_root",
            seed.id
        ))
    })?;

    ensure_private_dir(bot_root)?;
    if let Some(ws) = paths.bot_workspace_dir() {
        ensure_private_dir(&ws)?;
    }
    if let Some(inbox) = paths.bot_inbox_dir() {
        ensure_private_dir(&inbox)?;
    }
    if let Some(outbox) = paths.bot_outbox_dir() {
        ensure_private_dir(&outbox)?;
    }

    if let Some(soul) = paths.soul_md() {
        create_if_absent(&soul, read_asset_bytes("upstream/SOUL.md")?, &mut report)?;
    }
    if let Some(agents) = paths.agents_md() {
        create_if_absent(
            &agents,
            read_asset_bytes("upstream/AGENTS.md")?,
            &mut report,
        )?;
    }
    if let Some(tools) = paths.tools_md() {
        create_if_absent(&tools, read_asset_bytes("upstream/TOOLS.md")?, &mut report)?;
    }
    if let Some(heartbeat) = paths.heartbeat_md() {
        create_if_absent(
            &heartbeat,
            read_asset_bytes("upstream/HEARTBEAT.md")?,
            &mut report,
        )?;
    }

    if let Some(identity) = paths.identity_md() {
        if path_exists_no_follow(&identity) {
            let existing = std::fs::read_to_string(&identity)?;
            let updated = sync_identity_markdown(&existing, seed);
            if updated != existing {
                atomic_write_private(&identity, updated.as_bytes())?;
                report.refreshed.push(identity);
            } else {
                report.preserved.push(identity);
            }
        } else {
            let mut content = read_asset_str("upstream/IDENTITY.md")?.to_string();
            if !content.ends_with('\n') {
                content.push('\n');
            }
            content.push_str("\n## Role in this team\n\n");
            let role_asset = format!("grok_muse/roles/{}.md", seed.id);
            if let Ok(built_in_role) = read_asset_str(&role_asset) {
                content.push_str(built_in_role.trim());
                content.push('\n');
            } else {
                content.push_str(&format!(
                    "- **Bot ID:** `{}`\n- **Display Name:** {}\n- **Role:** {}\n",
                    seed.id, seed.display_name, seed.role_title
                ));
            }
            let legacy = seed.legacy_system_prompt.trim();
            if !legacy.is_empty() {
                content.push_str("\n## Legacy instructions\n\n");
                content.push_str(legacy);
                content.push('\n');
            }
            atomic_write_private(&identity, content.as_bytes())?;
            report.created.push(identity);
        }
    }

    Ok(report)
}

/// Execute idempotent schema migrations and persist `<storage>/.muse_schema_version`.
pub fn run_migrations(paths: &MuseRuntimePaths) -> Result<u32> {
    ensure_private_dir(paths.storage_dir())?;
    let version_file = paths.schema_version_path();
    let current = if path_exists_no_follow(&version_file) {
        let raw = std::fs::read_to_string(&version_file)?;
        raw.trim().parse::<u32>().unwrap_or(0)
    } else {
        0
    };

    if current < CURRENT_SCHEMA_VERSION {
        let _ = bootstrap_shared_runtime_files(paths)?;
        atomic_write_private(
            &version_file,
            format!("{CURRENT_SCHEMA_VERSION}\n").as_bytes(),
        )?;
    } else {
        // Ensure bundled read-only skills/ and docs/ stay up to date across binary upgrades.
        let mut dummy = BootstrapReport::default();
        refresh_readonly_dir_tree(
            paths.storage_dir(),
            "upstream/skills",
            &paths.skills_dir(),
            &mut dummy,
        )?;
        refresh_readonly_dir_tree(
            paths.storage_dir(),
            "upstream/docs",
            &paths.docs_dir(),
            &mut dummy,
        )?;
    }

    Ok(CURRENT_SCHEMA_VERSION)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;

    #[test]
    fn upstream_checksums_match_embedded_files_verbatim() {
        let manifest = read_asset_str("UPSTREAM.md").expect("UPSTREAM.md missing");
        let mut expected: BTreeMap<String, String> = BTreeMap::new();
        for line in manifest.lines() {
            if let Some(rest) = line.strip_prefix("| `")
                && let Some((hash, tail)) = rest.split_once("` | `")
                && let Some(path) = tail.strip_suffix("` |")
            {
                expected.insert(path.to_string(), hash.to_string());
            }
        }
        assert!(
            expected.len() >= 100,
            "Expected at least 100 entries in UPSTREAM.md, found {}",
            expected.len()
        );

        fn collect_upstream(dir: &Dir<'_>, actual: &mut BTreeMap<String, String>) {
            for f in dir.files() {
                let rel = f
                    .path()
                    .strip_prefix("upstream")
                    .expect("upstream prefix")
                    .to_string_lossy()
                    .replace('\\', "/");
                let mut hasher = Sha256::new();
                hasher.update(f.contents());
                let digest = hex::encode(hasher.finalize());
                actual.insert(rel, digest);
            }
            for sub in dir.dirs() {
                collect_upstream(sub, actual);
            }
        }

        let upstream_dir = ASSETS.get_dir("upstream").expect("upstream dir missing");
        let mut actual = BTreeMap::new();
        collect_upstream(upstream_dir, &mut actual);

        assert_eq!(
            actual, expected,
            "Upstream asset checksum mismatch detected"
        );
    }

    #[test]
    fn bootstrap_idempotent_preserves_user_edits_and_refreshes_skills() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = tmp.path().join("storage");
        let bot_root = storage.join("bots/muse");
        let paths = MuseRuntimePaths::for_bot(&storage, &bot_root, "muse");

        let seed = BotSeed {
            id: "muse",
            display_name: "Muse Operator",
            role_title: "Lead Operator",
            legacy_system_prompt: "Old inline prompt block",
        };

        let r1 = bootstrap_shared_runtime_files(&paths).unwrap();
        assert!(!r1.created.is_empty());
        assert!(paths.skills_dir().join("goals/SKILL.md").exists());
        assert!(paths.docs_dir().join("muse.md").exists());

        let b1 = bootstrap_bot_runtime_files(&paths, &seed).unwrap();
        assert_eq!(b1.created.len(), 5);
        let identity_text = std::fs::read_to_string(paths.identity_md().unwrap()).unwrap();
        assert!(identity_text.contains("## Role in this team"));
        assert!(identity_text.contains("## Legacy instructions"));
        assert!(identity_text.contains("Old inline prompt block"));

        // Second run creates zero user/bot files and preserves existing ones
        let r2 = bootstrap_shared_runtime_files(&paths).unwrap();
        assert!(r2.created.is_empty());
        assert!(!r2.preserved.is_empty());
        let b2 = bootstrap_bot_runtime_files(&paths, &seed).unwrap();
        assert!(b2.created.is_empty());
        assert_eq!(b2.preserved.len(), 5);

        // User edit to USER.md is preserved
        std::fs::write(paths.user_md(), "# USER.md\nCustom Name: Alex\n").unwrap();
        // Tampering inside read-only skills/ is overwritten on refresh
        std::fs::write(paths.skills_dir().join("tampered.txt"), "bad").unwrap();

        let _r3 = bootstrap_shared_runtime_files(&paths).unwrap();
        let user_after = std::fs::read_to_string(paths.user_md()).unwrap();
        assert!(user_after.contains("Custom Name: Alex"));
        assert!(!paths.skills_dir().join("tampered.txt").exists());
        assert!(paths.skills_dir().join("goals/SKILL.md").exists());

        // User-authored sections in IDENTITY.md are preserved while migrated Legacy instructions
        // are replaced or cleared when the bot profile changes.
        let id_path = paths.identity_md().unwrap();
        let mut authored = std::fs::read_to_string(&id_path).unwrap();
        authored.push_str("\n## Authored Personality\n\nDry wit and concise summaries.\n");
        std::fs::write(&id_path, &authored).unwrap();

        let seed_changed = BotSeed {
            legacy_system_prompt: "Updated inline prompt block",
            ..seed
        };
        bootstrap_bot_runtime_files(&paths, &seed_changed).unwrap();
        let id_after_replace = std::fs::read_to_string(&id_path).unwrap();
        assert!(id_after_replace.contains("## Authored Personality"));
        assert!(id_after_replace.contains("Dry wit and concise summaries."));
        assert!(id_after_replace.contains("Updated inline prompt block"));
        assert!(!id_after_replace.contains("Old inline prompt block"));

        let seed_cleared = BotSeed {
            legacy_system_prompt: "",
            ..seed
        };
        bootstrap_bot_runtime_files(&paths, &seed_cleared).unwrap();
        let id_after_clear = std::fs::read_to_string(&id_path).unwrap();
        assert!(id_after_clear.contains("Dry wit and concise summaries."));
        assert!(!id_after_clear.contains("## Legacy instructions"));
        assert!(!id_after_clear.contains("Updated inline prompt block"));

        // Migrations are idempotent
        assert_eq!(run_migrations(&paths).unwrap(), CURRENT_SCHEMA_VERSION);
        assert_eq!(run_migrations(&paths).unwrap(), CURRENT_SCHEMA_VERSION);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir_mode = std::fs::metadata(paths.skills_dir())
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(dir_mode, 0o700);
            let file_mode = std::fs::metadata(paths.user_md())
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(file_mode, 0o600);
            let skill_file_mode = std::fs::metadata(paths.skills_dir().join("goals/SKILL.md"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(skill_file_mode, 0o600);
        }
    }
}
