use chrono::Utc;
use gm_agent::BotWorkspaceSandbox;
use gm_core::error::{GmError, Result};
use gm_core::muse_paths::MuseRuntimePaths;
use gm_memory::PersonalMemoryStore;
use gm_muse::templates::{BotSeed, bootstrap_bot_runtime_files, run_migrations};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn default_true() -> bool {
    true
}

fn default_avatar_shape() -> String {
    "Circle".into()
}

fn default_avatar_color() -> String {
    "#2F7CF6".into()
}

fn default_task_mode() -> String {
    "chat".into()
}

pub const AVATAR_SHAPES: &[&str] = &["Circle", "Hex", "RoundedSquare", "Diamond", "Pill", "Spark"];

pub const BOT_PALETTE_HEX: &[&str] = &[
    "#2F7CF6", // Electric Blue
    "#10B981", // Emerald
    "#F59E0B", // Amber
    "#8B5CF6", // Violet
    "#F43F5E", // Rose
    "#14B8A6", // Teal
    "#6366F1", // Indigo
    "#FB7185", // Coral
    "#64748B", // Slate
    "#06B6D4", // Cyan
];

pub const MAX_AVATAR_BYTES: usize = 25 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BotProfile {
    pub id: String,
    pub display_name: String,
    pub avatar_emoji: String,
    pub role_title: String,
    pub system_prompt: String,
    pub preferred_provider: Option<String>,
    pub preferred_model: Option<String>,
    pub allow_gui: bool,
    pub allow_shell: bool,
    pub allow_network: bool,
    pub created_at: String,
    #[serde(default)]
    pub is_primary: bool,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub section_id: Option<String>,
    #[serde(default)]
    pub hidden_from_sidebar: bool,
    #[serde(default = "default_avatar_shape")]
    pub avatar_shape: String,
    #[serde(default = "default_avatar_color")]
    pub avatar_color: String,
    #[serde(default)]
    pub avatar_asset: Option<String>,
    #[serde(default)]
    pub accent_color: Option<String>,
    #[serde(default = "default_true")]
    pub notifications_enabled: bool,
    #[serde(default = "default_true")]
    pub drafts_enabled: bool,
    #[serde(default = "default_task_mode")]
    pub task_mode_default: String,
}

impl Default for BotProfile {
    fn default() -> Self {
        Self {
            id: String::new(),
            display_name: String::new(),
            avatar_emoji: "🤖".into(),
            role_title: String::new(),
            system_prompt: String::new(),
            preferred_provider: None,
            preferred_model: None,
            allow_gui: false,
            allow_shell: false,
            allow_network: false,
            created_at: Utc::now().to_rfc3339(),
            is_primary: false,
            pinned: false,
            section_id: None,
            hidden_from_sidebar: false,
            avatar_shape: default_avatar_shape(),
            avatar_color: default_avatar_color(),
            avatar_asset: None,
            accent_color: None,
            notifications_enabled: true,
            drafts_enabled: true,
            task_mode_default: default_task_mode(),
        }
    }
}

/// Persistent roster of OpenBot AI teammates with isolated workspace folders and per-bot memory.
#[derive(Clone)]
pub struct BotRoster {
    storage_dir: PathBuf,
    bots_dir: PathBuf,
    roster_file: PathBuf,
    bots: Arc<Mutex<BTreeMap<String, BotProfile>>>,
}

impl BotRoster {
    pub fn open(bots_dir: impl AsRef<Path>) -> Result<Self> {
        let dir = bots_dir.as_ref().to_path_buf();
        let storage_dir = if dir.file_name().and_then(|s| s.to_str()) == Some("bots") {
            dir.parent().unwrap_or(&dir).to_path_buf()
        } else {
            dir.clone()
        };
        Self::open_with_storage(dir, storage_dir)
    }

    pub fn open_with_storage(
        bots_dir: impl AsRef<Path>,
        storage_dir: impl AsRef<Path>,
    ) -> Result<Self> {
        let dir = bots_dir.as_ref().to_path_buf();
        let storage = storage_dir.as_ref().to_path_buf();
        gm_core::storage::ensure_private_dir(&dir)?;
        let _lease = Self::lock_directory(&dir)?;
        let roster_file = dir.join("roster.json");

        let mut loaded: BTreeMap<String, BotProfile> = if roster_file.exists() {
            let content = std::fs::read_to_string(&roster_file)?;
            // A damaged roster must never be replaced by defaults on the next read.
            serde_json::from_str(&content)?
        } else {
            BTreeMap::new()
        };

        for (key, profile) in &loaded {
            if key != &profile.id
                || key.is_empty()
                || key.len() > 64
                || !key
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
            {
                return Err(GmError::Config(
                    "Roster contains an invalid bot identifier; original file preserved".into(),
                ));
            }
        }

        if loaded.is_empty() {
            for profile in Self::default_coworkers() {
                loaded.insert(profile.id.clone(), profile);
            }
        }
        Self::normalize_primary_invariant(&mut loaded);

        let roster = Self {
            storage_dir: storage,
            bots_dir: dir,
            roster_file,
            bots: Arc::new(Mutex::new(loaded)),
        };
        roster.persist()?;
        roster.provision_all_workspaces()?;
        Ok(roster)
    }

    fn normalize_primary_invariant(map: &mut BTreeMap<String, BotProfile>) {
        let primary_id = map
            .values()
            .find(|b| b.is_primary)
            .map(|b| b.id.clone())
            .or_else(|| map.contains_key("muse").then(|| "muse".to_string()))
            .or_else(|| map.keys().next().cloned());

        if let Some(pid) = primary_id {
            for (k, profile) in map.iter_mut() {
                profile.is_primary = *k == pid;
            }
        }
    }

    pub fn default_coworkers() -> Vec<BotProfile> {
        let now = Utc::now().to_rfc3339();
        vec![
            BotProfile {
                id: "muse".into(),
                display_name: "Muse Operator".into(),
                avatar_emoji: "⚡".into(),
                role_title: "General Desktop Operator & Coordinator".into(),
                system_prompt: "You are Muse, the lead desktop operator. Coordinate tasks, navigate macOS apps via Accessibility and Set-of-Marks vision, and delegate coding or deep research to @coder or @researcher when appropriate.".into(),
                preferred_provider: None,
                preferred_model: None,
                allow_gui: true,
                allow_shell: true,
                allow_network: true,
                created_at: now.clone(),
                is_primary: true,
                pinned: true,
                avatar_shape: "Hex".into(),
                avatar_color: "#2F7CF6".into(),
                ..Default::default()
            },
            BotProfile {
                id: "researcher".into(),
                display_name: "Nova Researcher".into(),
                avatar_emoji: "🔭".into(),
                role_title: "Deep Knowledge & Web Researcher".into(),
                system_prompt: "You are Nova, a meticulous research specialist. Synthesize documents, search memory and MCP sources, verify citations, and write structured briefs into your workspace/outbox.".into(),
                preferred_provider: None,
                preferred_model: None,
                allow_gui: false,
                allow_shell: true,
                allow_network: true,
                created_at: now.clone(),
                avatar_shape: "Circle".into(),
                avatar_color: "#8B5CF6".into(),
                ..Default::default()
            },
            BotProfile {
                id: "coder".into(),
                display_name: "Atlas Engineer".into(),
                avatar_emoji: "🛠️".into(),
                role_title: "Sandboxed Workspace Software Engineer".into(),
                system_prompt: "You are Atlas, a software engineer operating in an isolated project workspace. Write clean code, generate unified diff previews, run tests via WorkspaceShell, and place deliverables in outbox/.".into(),
                preferred_provider: None,
                preferred_model: None,
                allow_gui: false,
                allow_shell: true,
                allow_network: true,
                created_at: now.clone(),
                avatar_shape: "RoundedSquare".into(),
                avatar_color: "#10B981".into(),
                ..Default::default()
            },
            BotProfile {
                id: "guardian".into(),
                display_name: "Aegis Auditor".into(),
                avatar_emoji: "🛡️".into(),
                role_title: "Risk, Security & Governance Analyst".into(),
                system_prompt: "You are Aegis, a security and compliance analyst. Inspect audit logs, review workspace diffs, flag irreversible or risky operations, and never mutate external systems.".into(),
                preferred_provider: None,
                preferred_model: None,
                allow_gui: false,
                allow_shell: false,
                allow_network: false,
                created_at: now,
                avatar_shape: "Diamond".into(),
                avatar_color: "#F59E0B".into(),
                ..Default::default()
            },
        ]
    }

    fn lock_directory(directory: &Path) -> Result<std::fs::File> {
        let mut options = std::fs::OpenOptions::new();
        options.create(true).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let file = options.open(directory.join(".roster.lock"))?;
        file.lock()?;
        Ok(file)
    }

    pub fn refresh(&self) -> Result<()> {
        let raw = std::fs::read_to_string(&self.roster_file)?;
        let map: BTreeMap<String, BotProfile> = serde_json::from_str(&raw)?;
        for (key, profile) in &map {
            if key != &profile.id
                || key.is_empty()
                || key.len() > 64
                || !key
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
            {
                return Err(GmError::Config("Invalid bot identifier in roster".into()));
            }
        }
        *self.bots.lock() = map;
        Ok(())
    }

    fn persist(&self) -> Result<()> {
        let map = self.bots.lock();
        let json = serde_json::to_string_pretty(&*map)?;
        gm_core::storage::atomic_write_private(&self.roster_file, json.as_bytes())?;
        Ok(())
    }

    pub fn storage_dir(&self) -> &Path {
        &self.storage_dir
    }

    pub fn runtime_paths_for_bot(&self, bot_id: &str) -> MuseRuntimePaths {
        MuseRuntimePaths::for_bot(&self.storage_dir, self.bot_root(bot_id), bot_id)
    }

    fn bootstrap_bot_profile(&self, profile: &BotProfile) -> Result<()> {
        let paths = self.runtime_paths_for_bot(&profile.id);
        let seed = BotSeed {
            id: &profile.id,
            display_name: &profile.display_name,
            role_title: &profile.role_title,
            legacy_system_prompt: &profile.system_prompt,
        };
        bootstrap_bot_runtime_files(&paths, &seed)?;
        Ok(())
    }

    fn provision_all_workspaces(&self) -> Result<()> {
        run_migrations(&MuseRuntimePaths::shared(&self.storage_dir))?;
        let profiles: Vec<BotProfile> = self.bots.lock().values().cloned().collect();
        for profile in profiles {
            let bot_root = self.bot_root(&profile.id);
            let paths = self.runtime_paths_for_bot(&profile.id);
            let _ = BotWorkspaceSandbox::new(&profile.id, &bot_root)?
                .with_network(profile.allow_network)
                .with_shared_runtime_paths(paths);
            self.bootstrap_bot_profile(&profile)?;
        }
        Ok(())
    }

    pub fn bots_dir(&self) -> &Path {
        &self.bots_dir
    }

    pub fn bot_root(&self, bot_id: &str) -> PathBuf {
        self.bots_dir.join(bot_id)
    }

    pub fn workspace_sandbox(&self, bot_id: &str) -> Result<BotWorkspaceSandbox> {
        let profile = self
            .get_bot(bot_id)
            .ok_or_else(|| GmError::Config(format!("Unknown bot '{bot_id}'")))?;
        let sandbox = BotWorkspaceSandbox::new(&profile.id, self.bot_root(&profile.id))?
            .with_network(profile.allow_network)
            .with_shared_runtime_paths(self.runtime_paths_for_bot(&profile.id));
        Ok(sandbox)
    }

    pub fn memory_store(&self, bot_id: &str) -> Result<PersonalMemoryStore> {
        let root = self.bot_root(bot_id);
        std::fs::create_dir_all(&root)?;
        PersonalMemoryStore::open(root.join("memory.sqlite"))
    }

    pub fn list_bots(&self) -> Vec<BotProfile> {
        if let Err(error) = self.refresh() {
            tracing::error!(%error,"Roster could not be refreshed; admission disabled");
            return Vec::new();
        }
        self.bots.lock().values().cloned().collect()
    }

    pub fn get_bot(&self, bot_id: &str) -> Option<BotProfile> {
        self.refresh().ok()?;
        self.bots.lock().get(bot_id).cloned()
    }

    fn validate_ui_fields(profile: &BotProfile) -> Result<()> {
        if !AVATAR_SHAPES.contains(&profile.avatar_shape.as_str()) {
            return Err(GmError::Config(format!(
                "Unsupported avatar_shape '{}'",
                profile.avatar_shape
            )));
        }
        if profile.avatar_color.trim().is_empty()
            || profile.avatar_color.len() > 32
            || profile.avatar_color.contains('\n')
        {
            return Err(GmError::Config("Invalid avatar_color".into()));
        }
        if let Some(ref accent) = profile.accent_color
            && (accent.trim().is_empty() || accent.len() > 32 || accent.contains('\n'))
        {
            return Err(GmError::Config("Invalid accent_color".into()));
        }
        if !matches!(profile.task_mode_default.as_str(), "chat" | "queue") {
            return Err(GmError::Config(
                "task_mode_default must be 'chat' or 'queue'".into(),
            ));
        }
        if let Some(ref sec) = profile.section_id
            && (sec.trim().is_empty() || sec.len() > 64 || sec.contains('\n'))
        {
            return Err(GmError::Config("Invalid section_id".into()));
        }
        if let Some(ref asset) = profile.avatar_asset
            && (asset.is_empty()
                || asset.len() > 128
                || asset.contains('/')
                || asset.contains('\\')
                || asset.contains("..")
                || asset.contains('\0'))
        {
            return Err(GmError::SafetyViolation(
                "RULE-PATH-002: avatar_asset must be a safe flat filename".into(),
            ));
        }
        Ok(())
    }

    pub fn upsert_bot(&self, profile: BotProfile) -> Result<BotProfile> {
        let _lease = Self::lock_directory(&self.bots_dir)?;
        self.refresh()?;
        let id = profile.id.trim().to_lowercase();
        if id.is_empty()
            || id.len() > 64
            || !id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
        {
            return Err(GmError::Config(format!("Invalid bot ID '{}'", profile.id)));
        }
        if profile.display_name.trim().is_empty()
            || profile.display_name.len() > 256
            || profile.role_title.len() > 512
            || profile.system_prompt.len() > 64 * 1024
            || profile.avatar_emoji.len() > 64
            || profile
                .preferred_model
                .as_ref()
                .is_some_and(|s| s.len() > 256)
            || profile.preferred_provider.as_ref().is_some_and(|s| {
                !matches!(
                    s.as_str(),
                    "meta" | "muse" | "xai" | "grok" | "claude" | "codex" | "anthropic" | "openai"
                )
            })
        {
            return Err(GmError::Config(
                "Bot profile fields exceed limits or the preferred provider is unsupported".into(),
            ));
        }
        if profile.preferred_model.is_some() && profile.preferred_provider.is_none() {
            return Err(GmError::Config(
                "Choose a provider when setting a preferred model".into(),
            ));
        }
        Self::validate_ui_fields(&profile)?;

        let mut normalized = profile;
        normalized.id = id.clone();
        let paths = self.runtime_paths_for_bot(&id);
        let _ = BotWorkspaceSandbox::new(&id, self.bot_root(&id))?
            .with_network(normalized.allow_network)
            .with_shared_runtime_paths(paths);
        self.bootstrap_bot_profile(&normalized)?;
        let mut updated = self.bots.lock().clone();
        if normalized.is_primary {
            for b in updated.values_mut() {
                b.is_primary = false;
            }
        }
        updated.insert(id.clone(), normalized.clone());
        Self::normalize_primary_invariant(&mut updated);
        let stored = updated.get(&id).cloned().unwrap_or(normalized);
        gm_core::storage::atomic_write_private(
            &self.roster_file,
            &serde_json::to_vec_pretty(&updated)?,
        )?;
        *self.bots.lock() = updated;
        Ok(stored)
    }

    pub fn save_bot_ui_prefs(&self, bot_id: &str, prefs: &serde_json::Value) -> Result<BotProfile> {
        let mut profile = self
            .get_bot(bot_id)
            .ok_or_else(|| GmError::Config(format!("Unknown bot '{bot_id}'")))?;

        if let Some(v) = prefs.get("display_name").and_then(|v| v.as_str()) {
            profile.display_name = v.trim().to_string();
        }
        if let Some(v) = prefs.get("role_title").and_then(|v| v.as_str()) {
            profile.role_title = v.trim().to_string();
        }
        if let Some(v) = prefs.get("avatar_emoji").and_then(|v| v.as_str()) {
            profile.avatar_emoji = v.trim().to_string();
        }
        if let Some(v) = prefs.get("is_primary").and_then(|v| v.as_bool()) {
            profile.is_primary = v;
        }
        if let Some(v) = prefs.get("pinned").and_then(|v| v.as_bool()) {
            profile.pinned = v;
        }
        if let Some(v) = prefs.get("section_id") {
            profile.section_id = v
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
        }
        if let Some(v) = prefs.get("hidden_from_sidebar").and_then(|v| v.as_bool()) {
            profile.hidden_from_sidebar = v;
        }
        if let Some(v) = prefs.get("avatar_shape").and_then(|v| v.as_str()) {
            profile.avatar_shape = v.trim().to_string();
        }
        if let Some(v) = prefs.get("avatar_color").and_then(|v| v.as_str()) {
            profile.avatar_color = v.trim().to_string();
        }
        if let Some(v) = prefs.get("avatar_asset") {
            profile.avatar_asset = v
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
        }
        if let Some(v) = prefs.get("accent_color") {
            profile.accent_color = v
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
        }
        if let Some(v) = prefs.get("notifications_enabled").and_then(|v| v.as_bool()) {
            profile.notifications_enabled = v;
        }
        if let Some(v) = prefs.get("drafts_enabled").and_then(|v| v.as_bool()) {
            profile.drafts_enabled = v;
        }
        if let Some(v) = prefs.get("task_mode_default").and_then(|v| v.as_str()) {
            profile.task_mode_default = v.trim().to_string();
        }
        if let Some(v) = prefs.get("allow_gui").and_then(|v| v.as_bool()) {
            profile.allow_gui = v;
        }
        if let Some(v) = prefs.get("allow_shell").and_then(|v| v.as_bool()) {
            profile.allow_shell = v;
        }
        if let Some(v) = prefs.get("allow_network").and_then(|v| v.as_bool()) {
            profile.allow_network = v;
        }
        if let Some(v) = prefs.get("preferred_provider") {
            profile.preferred_provider = v
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
        }
        if let Some(v) = prefs.get("preferred_model") {
            profile.preferred_model = v
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
        }

        self.upsert_bot(profile)
    }

    pub fn duplicate_bot(
        &self,
        source_id: &str,
        new_id: Option<&str>,
        new_display_name: Option<&str>,
    ) -> Result<BotProfile> {
        let source = self
            .get_bot(source_id)
            .ok_or_else(|| GmError::Config(format!("Unknown bot '{source_id}'")))?;
        let candidate_id = new_id
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_lowercase())
            .unwrap_or_else(|| {
                let short = &uuid::Uuid::new_v4().simple().to_string()[..6];
                format!("{}_copy_{short}", source.id)
            });
        if self.get_bot(&candidate_id).is_some() {
            return Err(GmError::Config(format!(
                "Bot '{candidate_id}' already exists"
            )));
        }
        let cloned = BotProfile {
            id: candidate_id,
            display_name: new_display_name
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| format!("{} (Copy)", source.display_name)),
            created_at: Utc::now().to_rfc3339(),
            is_primary: false,
            avatar_asset: None,
            ..source
        };
        self.upsert_bot(cloned)
    }

    pub fn save_bot_avatar_bytes(
        &self,
        bot_id: &str,
        filename_hint: &str,
        bytes: &[u8],
    ) -> Result<String> {
        let profile = self
            .get_bot(bot_id)
            .ok_or_else(|| GmError::Config(format!("Unknown bot '{bot_id}'")))?;
        if bytes.is_empty() || bytes.len() > MAX_AVATAR_BYTES {
            return Err(GmError::Config(
                "Avatar image must be non-empty and under 25 MiB".into(),
            ));
        }
        let ext = Path::new(filename_hint)
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase())
            .filter(|s| matches!(s.as_str(), "png" | "jpg" | "jpeg" | "webp"))
            .unwrap_or_else(|| "png".to_string());
        let avatar_dir = self.bot_root(&profile.id).join("avatar");
        gm_core::storage::ensure_private_dir(&avatar_dir)?;
        let fname = format!(
            "avatar_{}.{ext}",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        );
        let target = avatar_dir.join(&fname);
        gm_core::storage::atomic_write_private(&target, bytes)?;
        let mut updated = profile;
        updated.avatar_asset = Some(fname.clone());
        self.upsert_bot(updated)?;
        Ok(fname)
    }

    pub fn read_bot_avatar_bytes(&self, bot_id: &str) -> Result<Option<(String, Vec<u8>)>> {
        let Some(profile) = self.get_bot(bot_id) else {
            return Ok(None);
        };
        let Some(asset) = profile.avatar_asset else {
            return Ok(None);
        };
        Self::validate_ui_fields(&BotProfile {
            avatar_asset: Some(asset.clone()),
            ..profile
        })?;
        let path = self.bot_root(bot_id).join("avatar").join(&asset);
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            return Ok(None);
        };
        if meta.file_type().is_symlink()
            || !meta.is_file()
            || meta.len() as usize > MAX_AVATAR_BYTES
        {
            return Err(GmError::SafetyViolation(
                "RULE-PATH-002: Avatar file invalid or symlink".into(),
            ));
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        use std::io::Read;
        let file = options.open(&path)?;
        let mut buf = Vec::new();
        file.take((MAX_AVATAR_BYTES + 1) as u64)
            .read_to_end(&mut buf)?;
        if buf.len() > MAX_AVATAR_BYTES {
            return Err(GmError::Config("Avatar exceeds 25 MiB limit".into()));
        }
        Ok(Some((asset, buf)))
    }

    pub fn remove_bot(&self, bot_id: &str) -> Result<bool> {
        let _lease = Self::lock_directory(&self.bots_dir)?;
        self.refresh()?;
        if bot_id == "muse" {
            return Err(GmError::Config(
                "Cannot remove the default 'muse' operator bot".into(),
            ));
        }
        let mut map = self.bots.lock().clone();
        let removed = map.remove(bot_id).is_some();
        if removed {
            Self::normalize_primary_invariant(&mut map);
            gm_core::storage::atomic_write_private(
                &self.roster_file,
                &serde_json::to_vec_pretty(&map)?,
            )?;
            *self.bots.lock() = map;
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gm_agent::{InteractionHandler, NonInteractiveHandler, WorkspaceToolCall};
    use gm_safety::KillSwitch;

    #[tokio::test]
    async fn roster_provisions_muse_runtime_and_attaches_shared_paths_to_sandbox() {
        let base = std::env::temp_dir().join(format!("ob_roster_muse_{}", uuid::Uuid::new_v4()));
        let bots_dir = base.join("bots");
        let roster = BotRoster::open_with_storage(&bots_dir, &base).unwrap();

        assert!(base.join("USER.md").exists());
        assert!(base.join("skills/goals/SKILL.md").exists());
        assert!(bots_dir.join("muse/SOUL.md").exists());
        assert!(bots_dir.join("coder/IDENTITY.md").exists());

        let sandbox = roster.workspace_sandbox("muse").unwrap();
        assert!(sandbox.shared_runtime_paths().is_some());

        let handler: Arc<dyn InteractionHandler> = Arc::new(NonInteractiveHandler);
        let ks = KillSwitch::new();
        let err = sandbox
            .execute_tool(
                "t",
                1,
                &WorkspaceToolCall::WriteFile {
                    path: "~/skills/x".into(),
                    content: "bad".into(),
                },
                &handler,
                &ks,
                None,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("RULE-PATH-002"));

        sandbox
            .execute_tool(
                "t",
                2,
                &WorkspaceToolCall::WriteFile {
                    path: "~/MEMORY.md".into(),
                    content: "# MEMORY.md\nSaved from sandbox.\n".into(),
                },
                &handler,
                &ks,
                None,
            )
            .await
            .unwrap();
        assert!(
            std::fs::read_to_string(base.join("MEMORY.md"))
                .unwrap()
                .contains("Saved from sandbox.")
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn roster_ui_prefs_primary_swap_duplicate_and_avatar_security() {
        let base = std::env::temp_dir().join(format!("ob_roster_ui_{}", uuid::Uuid::new_v4()));
        let roster = BotRoster::open_with_storage(base.join("bots"), &base).unwrap();

        assert!(roster.get_bot("muse").unwrap().is_primary);
        assert!(!roster.get_bot("coder").unwrap().is_primary);

        // Switching primary to coder clears primary from muse
        roster
            .save_bot_ui_prefs(
                "coder",
                &serde_json::json!({
                    "is_primary": true,
                    "pinned": true,
                    "avatar_shape": "Spark",
                    "avatar_color": "#10B981",
                    "hidden_from_sidebar": false,
                    "notifications_enabled": false
                }),
            )
            .unwrap();
        assert!(roster.get_bot("coder").unwrap().is_primary);
        assert!(!roster.get_bot("muse").unwrap().is_primary);
        assert_eq!(roster.get_bot("coder").unwrap().avatar_shape, "Spark");
        assert!(!roster.get_bot("coder").unwrap().notifications_enabled);

        // Removing primary bot restores primary back to muse automatically
        roster
            .save_bot_ui_prefs("muse", &serde_json::json!({"is_primary": true}))
            .unwrap();
        assert!(roster.get_bot("muse").unwrap().is_primary);

        // Duplicate bot
        let dup = roster
            .duplicate_bot("researcher", Some("researcher_v2"), Some("Nova 2"))
            .unwrap();
        assert_eq!(dup.id, "researcher_v2");
        assert_eq!(dup.display_name, "Nova 2");
        assert!(!dup.is_primary);

        // Avatar byte persistence + path traversal rejection
        let fname = roster
            .save_bot_avatar_bytes("muse", "badge.png", b"\x89PNG\r\n\x1a\nfake")
            .unwrap();
        let (read_name, read_bytes) = roster.read_bot_avatar_bytes("muse").unwrap().unwrap();
        assert_eq!(read_name, fname);
        assert_eq!(read_bytes, b"\x89PNG\r\n\x1a\nfake");

        assert!(
            roster
                .save_bot_ui_prefs(
                    "muse",
                    &serde_json::json!({"avatar_asset": "../escape.png"})
                )
                .is_err()
        );
        assert!(
            roster
                .save_bot_ui_prefs("muse", &serde_json::json!({"avatar_shape": "Triangle"}))
                .is_err()
        );

        let _ = std::fs::remove_dir_all(&base);
    }
}
