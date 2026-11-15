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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
        let mut normalized = profile;
        normalized.id = id.clone();
        let paths = self.runtime_paths_for_bot(&id);
        let _ = BotWorkspaceSandbox::new(&id, self.bot_root(&id))?
            .with_network(normalized.allow_network)
            .with_shared_runtime_paths(paths);
        self.bootstrap_bot_profile(&normalized)?;
        let mut updated = self.bots.lock().clone();
        updated.insert(id.clone(), normalized.clone());
        gm_core::storage::atomic_write_private(
            &self.roster_file,
            &serde_json::to_vec_pretty(&updated)?,
        )?;
        *self.bots.lock() = updated;
        Ok(normalized)
    }

    pub fn remove_bot(&self, bot_id: &str) -> Result<bool> {
        let _lease = Self::lock_directory(&self.bots_dir)?;
        self.refresh()?;
        if bot_id == "muse" {
            return Err(GmError::Config(
                "Cannot remove the default 'muse' operator bot".into(),
            ));
        }
        let removed = self.bots.lock().remove(bot_id).is_some();
        if removed {
            self.persist()?;
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
}
