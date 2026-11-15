//! Skills and connectors manifest loader, progressive-disclosure registry,
//! custom skill/connector scaffolding, and skill amendment management.

use gm_core::error::{GmError, Result};
use gm_core::muse_paths::MuseRuntimePaths;
use gm_core::storage::{atomic_write_private, ensure_private_dir};
use gm_safety::SecretScrubber;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Maximum permitted size for any `SKILL.md`, `CONNECTOR.md`, or amendment file (256 KiB).
pub const MAX_SKILL_DOC_BYTES: usize = 256 * 1024;

const DEFAULT_CREDENTIAL_GUIDANCE: &str = "Refused plaintext credential: store credentials as secret://keychain/<provider> or secret://1password/... references";

fn default_true() -> bool {
    true
}

fn skill_name_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[a-z0-9][a-z0-9-]{0,63}$").expect("valid skill regex"))
}

fn bundled_name_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[a-z0-9][a-z0-9_-]{0,63}$").expect("valid bundled skill regex"))
}

fn connector_id_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^custom\.[a-z0-9-]{1,63}$").expect("valid connector regex"))
}

fn plaintext_secret_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?x)
            \b(?:
                ghp_[A-Za-z0-9]{10,}
                |github_pat_[A-Za-z0-9_]{10,}
                |glpat-[A-Za-z0-9_-]{10,}
                |sk-(?:ant-|proj-|live-|test-)?[A-Za-z0-9_-]{12,}
                |xai-[A-Za-z0-9_-]{12,}
                |AKIA[0-9A-Z]{16}
                |AIza[0-9A-Za-z_-]{20,}
                |xox[baprs]-[A-Za-z0-9-]{10,}
            )\b
            |-----BEGIN\s+[A-Z\s]*PRIVATE\s+KEY-----
            |\bBearer\s+[A-Za-z0-9._-]{12,}
            ",
        )
        .expect("valid plaintext secret regex")
    })
}

pub fn is_valid_custom_skill_name(name: &str) -> bool {
    skill_name_regex().is_match(name)
}

pub fn is_valid_custom_connector_id(id: &str) -> bool {
    connector_id_regex().is_match(id)
}

fn is_valid_amendment_target(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0')
        && name != "."
        && name != ".."
        && !name.contains("..")
}

/// Refuse any plaintext credential tokens or registered live secrets in skill/connector text.
pub fn ensure_no_plaintext_secrets(text: &str, scrubber: &SecretScrubber) -> Result<()> {
    if scrubber.scrub_str(text) != text || plaintext_secret_regex().is_match(text) {
        return Err(GmError::SafetyViolation(
            DEFAULT_CREDENTIAL_GUIDANCE.to_string(),
        ));
    }
    Ok(())
}

/// Optional self-improvement metadata carried on system-managed custom skills.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillImprovementMeta {
    #[serde(default)]
    pub managed: bool,
    #[serde(default)]
    pub status: Option<String>,
}

/// Optional upstream-compatible base skill amendment header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillAmendsMeta {
    pub skill: String,
    pub reviewed_revision: String,
}

/// Optional upstream-compatible base connector amendment header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorAmendsMeta {
    pub connector: String,
    pub reviewed_revision: String,
}

/// Strict user-authored skill manifest (`custom-skills/<name>/skill.yaml`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillYaml {
    pub name: String,
    pub title: String,
    pub description: String,
    pub category: String,
    #[serde(default = "default_true")]
    pub include_in_prompt: bool,
    #[serde(default)]
    pub skill_improvement: Option<SkillImprovementMeta>,
    #[serde(default)]
    pub amends: Option<SkillAmendsMeta>,
}

/// Strict user-authored connector manifest (`custom-connectors/custom.<provider>/connector.yaml`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorYaml {
    pub id: String,
    pub title: String,
    pub description: String,
    #[serde(default = "default_true")]
    pub include_in_prompt: bool,
    #[serde(default = "default_true")]
    pub require_auth: bool,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub amends: Option<ConnectorAmendsMeta>,
}

/// Lenient bundled skill manifest (ignores extra upstream keys such as `availability`).
#[derive(Debug, Clone, Deserialize)]
struct BundledSkillYaml {
    name: String,
    title: String,
    description: String,
    category: String,
    #[serde(default = "default_true")]
    include_in_prompt: bool,
    #[serde(default)]
    skill_improvement: Option<SkillImprovementMeta>,
    #[serde(default)]
    amends: Option<SkillAmendsMeta>,
}

impl From<BundledSkillYaml> for SkillYaml {
    fn from(b: BundledSkillYaml) -> Self {
        Self {
            name: b.name,
            title: b.title,
            description: b.description,
            category: b.category,
            include_in_prompt: b.include_in_prompt,
            skill_improvement: b.skill_improvement,
            amends: b.amends,
        }
    }
}

/// Lenient bundled connector manifest.
#[derive(Debug, Clone, Deserialize)]
struct BundledConnectorYaml {
    id: String,
    title: String,
    description: String,
    #[serde(default = "default_true")]
    include_in_prompt: bool,
    #[serde(default = "default_true")]
    require_auth: bool,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    amends: Option<ConnectorAmendsMeta>,
}

impl From<BundledConnectorYaml> for ConnectorYaml {
    fn from(b: BundledConnectorYaml) -> Self {
        Self {
            id: b.id,
            title: b.title,
            description: b.description,
            include_in_prompt: b.include_in_prompt,
            require_auth: b.require_auth,
            aliases: b.aliases,
            amends: b.amends,
        }
    }
}

/// Parse a strict user-authored `skill.yaml` (`deny_unknown_fields` + name check).
pub fn parse_custom_skill_yaml(raw: &str) -> Result<SkillYaml> {
    let manifest: SkillYaml = serde_yaml_ng::from_str(raw)
        .map_err(|e| GmError::Config(format!("Invalid custom skill.yaml: {e}")))?;
    if !is_valid_custom_skill_name(&manifest.name) {
        return Err(GmError::Config(format!(
            "Invalid custom skill name '{}': must match ^[a-z0-9][a-z0-9-]{{0,63}}$",
            manifest.name
        )));
    }
    Ok(manifest)
}

/// Parse a strict user-authored `connector.yaml` (`deny_unknown_fields` + `custom.*` id check).
pub fn parse_custom_connector_yaml(raw: &str) -> Result<ConnectorYaml> {
    let manifest: ConnectorYaml = serde_yaml_ng::from_str(raw)
        .map_err(|e| GmError::Config(format!("Invalid custom connector.yaml: {e}")))?;
    if !is_valid_custom_connector_id(&manifest.id) {
        return Err(GmError::Config(format!(
            "Invalid custom connector id '{}': must match ^custom\\.[a-z0-9-]{{1,63}}$",
            manifest.id
        )));
    }
    Ok(manifest)
}

/// Indexed skill entry inside [`SkillRegistry`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillEntry {
    pub dir_name: String,
    pub manifest: SkillYaml,
    pub bundled: bool,
    pub root_dir: PathBuf,
    pub doc_path: Option<PathBuf>,
    pub reference_paths: Vec<String>,
}

/// Full skill payload returned by [`SkillRegistry::load_skill`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadedSkill {
    pub dir_name: String,
    pub manifest: SkillYaml,
    pub bundled: bool,
    pub skill_markdown: String,
    pub amendment_markdown: Option<String>,
    pub references: Vec<String>,
}

/// Indexed connector entry inside [`SkillRegistry`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectorEntry {
    pub dir_name: String,
    pub manifest: ConnectorYaml,
    pub bundled: bool,
    pub root_dir: PathBuf,
    pub doc_path: PathBuf,
    pub reference_paths: Vec<String>,
}

/// Full connector payload returned by [`SkillRegistry::load_connector`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadedConnector {
    pub dir_name: String,
    pub manifest: ConnectorYaml,
    pub bundled: bool,
    pub connector_markdown: String,
    pub amendment_markdown: Option<String>,
    pub references: Vec<String>,
}

/// Target kind for [`SkillRegistry::append_amendment`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AmendmentTarget {
    Skill,
    Connector,
}

/// Registry over bundled and custom Muse skills, connectors, and user amendments.
#[derive(Debug, Clone)]
pub struct SkillRegistry {
    paths: MuseRuntimePaths,
    scrubber: SecretScrubber,
    skills: Vec<SkillEntry>,
    connectors: Vec<ConnectorEntry>,
}

impl SkillRegistry {
    /// Strictly scan bundled and user-authored skills/connectors under `paths`.
    /// Fails immediately if any custom entry has an invalid name, unknown field,
    /// oversize doc (>256 KiB), symlink escape, or plaintext secret.
    pub fn open(paths: &MuseRuntimePaths) -> Result<Self> {
        Self::open_with_scrubber(paths, SecretScrubber::new())
    }

    /// Strict scan using an active [`SecretScrubber`] holding live leased secrets.
    pub fn open_with_scrubber(paths: &MuseRuntimePaths, scrubber: SecretScrubber) -> Result<Self> {
        Self::scan(paths, scrubber, true)
    }

    /// Fault-tolerant scan used by prompt assembly so one malformed custom file logs a
    /// warning without taking down chat or task execution.
    pub fn open_resilient(paths: &MuseRuntimePaths, scrubber: SecretScrubber) -> Self {
        Self::scan(paths, scrubber, false).unwrap_or_else(|err| {
            tracing::warn!("Resilient skill scan recovered from error: {err}");
            Self {
                paths: paths.clone(),
                scrubber: SecretScrubber::new(),
                skills: Vec::new(),
                connectors: Vec::new(),
            }
        })
    }

    fn scan(paths: &MuseRuntimePaths, scrubber: SecretScrubber, strict: bool) -> Result<Self> {
        let mut reg = Self {
            paths: paths.clone(),
            scrubber,
            skills: Vec::new(),
            connectors: Vec::new(),
        };
        reg.scan_bundled_skills(strict)?;
        reg.scan_custom_skills(strict)?;
        reg.scan_bundled_connectors(strict)?;
        reg.scan_custom_connectors(strict)?;
        reg.skills.sort_by(|a, b| a.dir_name.cmp(&b.dir_name));
        reg.connectors.sort_by(|a, b| a.dir_name.cmp(&b.dir_name));
        Ok(reg)
    }

    pub fn skills(&self) -> &[SkillEntry] {
        &self.skills
    }

    pub fn connectors(&self) -> &[ConnectorEntry] {
        &self.connectors
    }

    /// Return progressive-disclosure prompt inventory listing only skills with
    /// `include_in_prompt: true` (excluding retired ones) and connectors that either need
    /// no auth or are listed in `authenticated_connectors`.
    pub fn prompt_inventory(&self, authenticated_connectors: &[String]) -> String {
        let mut lines = Vec::new();
        for skill in &self.skills {
            if !skill.manifest.include_in_prompt {
                continue;
            }
            if skill
                .manifest
                .skill_improvement
                .as_ref()
                .and_then(|s| s.status.as_deref())
                == Some("retired")
            {
                continue;
            }
            let scope = if skill.bundled { "system" } else { "custom" };
            lines.push(format!(
                "- `{}` ({}, {}): {} [Load with LoadSkill]",
                skill.manifest.name, skill.manifest.title, scope, skill.manifest.description
            ));
        }
        for conn in &self.connectors {
            if !conn.manifest.include_in_prompt {
                continue;
            }
            if conn.manifest.require_auth
                && !is_connector_authenticated(conn, authenticated_connectors)
            {
                continue;
            }
            lines.push(format!(
                "- connector `{}` ({}): {} [Load with LoadConnector]",
                conn.manifest.id, conn.manifest.title, conn.manifest.description
            ));
        }
        lines.join("\n")
    }

    /// Search skills across name, directory name, title, description, and category.
    pub fn search_skills(&self, query: &str) -> Vec<&SkillEntry> {
        let q = query.trim().to_ascii_lowercase();
        if q.is_empty() {
            return self.skills.iter().collect();
        }
        let terms: Vec<&str> = q.split_whitespace().collect();
        self.skills
            .iter()
            .filter(|s| {
                let haystack = format!(
                    "{} {} {} {} {}",
                    s.dir_name,
                    s.manifest.name,
                    s.manifest.title,
                    s.manifest.description,
                    s.manifest.category
                )
                .to_ascii_lowercase();
                terms.iter().all(|t| haystack.contains(t))
            })
            .collect()
    }

    /// Load a skill's `SKILL.md` (or multi-guide index), any matching amendment, and its
    /// relative `references/` paths. Enforces the 256 KiB cap on user-authored files.
    pub fn load_skill(&self, name: &str) -> Result<LoadedSkill> {
        let q = name.trim();
        let entry = self
            .skills
            .iter()
            .find(|s| {
                s.dir_name.eq_ignore_ascii_case(q)
                    || s.manifest.name.eq_ignore_ascii_case(q)
                    || normalize_key(&s.dir_name) == normalize_key(q)
                    || normalize_key(&s.manifest.name) == normalize_key(q)
            })
            .ok_or_else(|| GmError::Config(format!("Skill '{name}' not found")))?;

        let mut skill_markdown = match &entry.doc_path {
            Some(path) => {
                let text = read_capped_text(path, MAX_SKILL_DOC_BYTES, !entry.bundled)?;
                if !entry.bundled {
                    ensure_no_plaintext_secrets(&text, &self.scrubber)?;
                }
                text
            }
            None => synthesize_bundle_guide(entry),
        };

        if entry.bundled && entry.dir_name == "spaces" {
            skill_markdown.push_str(
                "\n\n> **Grok-Muse Static Spaces Notice:** TypeScript Spaces (`kind: ts`) and remote runtime deployment are not available yet; create `kind: static` Spaces under `~/spaces/<slug>/`.\n",
            );
        }

        let amendment_markdown = self.read_skill_amendments(entry)?;

        Ok(LoadedSkill {
            dir_name: entry.dir_name.clone(),
            manifest: entry.manifest.clone(),
            bundled: entry.bundled,
            skill_markdown,
            amendment_markdown,
            references: entry.reference_paths.clone(),
        })
    }

    /// Search connectors across id, directory name, title, description, and aliases.
    pub fn search_connectors(&self, query: &str) -> Vec<&ConnectorEntry> {
        let q = query.trim().to_ascii_lowercase();
        if q.is_empty() {
            return self.connectors.iter().collect();
        }
        let terms: Vec<&str> = q.split_whitespace().collect();
        self.connectors
            .iter()
            .filter(|c| {
                let mut haystack = format!(
                    "{} {} {} {}",
                    c.dir_name, c.manifest.id, c.manifest.title, c.manifest.description
                );
                for alias in &c.manifest.aliases {
                    haystack.push(' ');
                    haystack.push_str(alias);
                }
                let lower = haystack.to_ascii_lowercase();
                terms.iter().all(|t| lower.contains(t))
            })
            .collect()
    }

    /// Load a connector doc by id, folder name, or alias, overlaying any amendment note.
    pub fn load_connector(&self, id_or_alias: &str) -> Result<LoadedConnector> {
        let q = id_or_alias.trim();
        let entry = self
            .connectors
            .iter()
            .find(|c| {
                c.dir_name.eq_ignore_ascii_case(q)
                    || c.manifest.id.eq_ignore_ascii_case(q)
                    || c.manifest
                        .aliases
                        .iter()
                        .any(|alias| alias.eq_ignore_ascii_case(q))
            })
            .ok_or_else(|| GmError::Config(format!("Connector '{id_or_alias}' not found")))?;

        let connector_markdown =
            read_capped_text(&entry.doc_path, MAX_SKILL_DOC_BYTES, !entry.bundled)?;
        if !entry.bundled {
            ensure_no_plaintext_secrets(&connector_markdown, &self.scrubber)?;
        }

        let amendment_markdown = self.read_connector_amendments(entry)?;

        Ok(LoadedConnector {
            dir_name: entry.dir_name.clone(),
            manifest: entry.manifest.clone(),
            bundled: entry.bundled,
            connector_markdown,
            amendment_markdown,
            references: entry.reference_paths.clone(),
        })
    }

    /// Create or update a user-authored custom skill under `custom-skills/<name>/`.
    pub fn scaffold_custom_skill(
        &mut self,
        manifest: SkillYaml,
        body_markdown: &str,
    ) -> Result<PathBuf> {
        if !is_valid_custom_skill_name(&manifest.name) {
            return Err(GmError::Config(format!(
                "Invalid custom skill name '{}': must match ^[a-z0-9][a-z0-9-]{{0,63}}$",
                manifest.name
            )));
        }
        if self.skills.iter().any(|s| {
            s.bundled
                && (s.dir_name == manifest.name
                    || s.manifest.name == manifest.name
                    || normalize_key(&s.dir_name) == normalize_key(&manifest.name))
        }) {
            return Err(GmError::Config(format!(
                "Skill name '{}' is reserved by a system skill; choose a distinct custom skill name or write an amendment",
                manifest.name
            )));
        }
        if body_markdown.len() > MAX_SKILL_DOC_BYTES {
            return Err(GmError::Config(format!(
                "SKILL.md size {} exceeds 256 KiB limit",
                body_markdown.len()
            )));
        }

        let yaml_text = serde_yaml_ng::to_string(&manifest)
            .map_err(|e| GmError::Config(format!("Failed to serialize skill.yaml: {e}")))?;
        ensure_no_plaintext_secrets(&yaml_text, &self.scrubber)?;
        ensure_no_plaintext_secrets(body_markdown, &self.scrubber)?;

        let dir = self.paths.custom_skills_dir().join(&manifest.name);
        ensure_private_dir(&self.paths.custom_skills_dir())?;
        ensure_private_dir(&dir)?;
        atomic_write_private(&dir.join("skill.yaml"), yaml_text.as_bytes())?;
        atomic_write_private(&dir.join("SKILL.md"), body_markdown.as_bytes())?;

        self.refresh()?;
        Ok(dir)
    }

    /// Create or update a user-authored custom connector under
    /// `custom-connectors/custom.<provider>/`.
    pub fn scaffold_custom_connector(
        &mut self,
        manifest: ConnectorYaml,
        body_markdown: &str,
    ) -> Result<PathBuf> {
        if !is_valid_custom_connector_id(&manifest.id) {
            return Err(GmError::Config(format!(
                "Invalid custom connector id '{}': must match ^custom\\.[a-z0-9-]{{1,63}}$",
                manifest.id
            )));
        }
        if body_markdown.len() > MAX_SKILL_DOC_BYTES {
            return Err(GmError::Config(format!(
                "CONNECTOR.md size {} exceeds 256 KiB limit",
                body_markdown.len()
            )));
        }

        let yaml_text = serde_yaml_ng::to_string(&manifest)
            .map_err(|e| GmError::Config(format!("Failed to serialize connector.yaml: {e}")))?;
        ensure_no_plaintext_secrets(&yaml_text, &self.scrubber)?;
        ensure_no_plaintext_secrets(body_markdown, &self.scrubber)?;

        let dir = self.paths.custom_connectors_dir().join(&manifest.id);
        ensure_private_dir(&self.paths.custom_connectors_dir())?;
        ensure_private_dir(&dir)?;
        atomic_write_private(&dir.join("connector.yaml"), yaml_text.as_bytes())?;
        atomic_write_private(&dir.join("CONNECTOR.md"), body_markdown.as_bytes())?;

        self.refresh()?;
        Ok(dir)
    }

    /// Append an amendment note under `workspace/skill-amendments/<target>.md` or
    /// `workspace/connector-amendments/<target>.md`.
    pub fn append_amendment(
        &self,
        kind: AmendmentTarget,
        target: &str,
        note: &str,
    ) -> Result<PathBuf> {
        if !is_valid_amendment_target(target) {
            return Err(GmError::SafetyViolation(format!(
                "RULE-PATH-002: Invalid amendment target '{target}'"
            )));
        }
        let trimmed = note.trim();
        if trimmed.is_empty() {
            return Err(GmError::Config("Amendment note cannot be empty".into()));
        }
        ensure_no_plaintext_secrets(trimmed, &self.scrubber)?;

        let dir = match kind {
            AmendmentTarget::Skill => self.paths.skill_amendments_dir(),
            AmendmentTarget::Connector => self.paths.connector_amendments_dir(),
        };
        ensure_private_dir(&dir)?;
        let file = dir.join(format!("{target}.md"));

        let existing = if file_exists_no_follow(&file)? {
            read_capped_text(&file, MAX_SKILL_DOC_BYTES, true)?
        } else {
            String::new()
        };
        let next = if existing.trim().is_empty() {
            format!("{trimmed}\n")
        } else {
            format!("{}\n\n{trimmed}\n", existing.trim_end())
        };
        if next.len() > MAX_SKILL_DOC_BYTES {
            return Err(GmError::Config(format!(
                "Amendment file exceeds 256 KiB limit ({} bytes)",
                next.len()
            )));
        }

        atomic_write_private(&file, next.as_bytes())?;
        Ok(file)
    }

    fn refresh(&mut self) -> Result<()> {
        let updated = Self::scan(&self.paths, self.scrubber.clone(), true)?;
        self.skills = updated.skills;
        self.connectors = updated.connectors;
        Ok(())
    }

    fn read_skill_amendments(&self, entry: &SkillEntry) -> Result<Option<String>> {
        let dir = self.paths.skill_amendments_dir();
        let mut chunks = Vec::new();
        let mut candidates = BTreeSet::new();
        candidates.insert(entry.dir_name.clone());
        candidates.insert(entry.manifest.name.clone());
        for cand in candidates {
            let p = dir.join(format!("{cand}.md"));
            if file_exists_no_follow(&p)? {
                let text = read_capped_text(&p, MAX_SKILL_DOC_BYTES, true)?;
                if !text.trim().is_empty() {
                    ensure_no_plaintext_secrets(&text, &self.scrubber)?;
                    chunks.push(text.trim().to_string());
                }
            }
        }
        // Also fold custom skills that declare `amends.skill` targeting this base skill.
        if entry.bundled {
            for custom in self.skills.iter().filter(|s| !s.bundled) {
                if let Some(ref amends) = custom.manifest.amends
                    && (amends.skill.eq_ignore_ascii_case(&entry.manifest.name)
                        || amends.skill.eq_ignore_ascii_case(&entry.dir_name))
                    && let Some(ref custom_doc) = custom.doc_path
                {
                    let body = read_capped_text(custom_doc, MAX_SKILL_DOC_BYTES, true)?;
                    ensure_no_plaintext_secrets(&body, &self.scrubber)?;
                    chunks.push(format!(
                        "### Amendment `{}` (reviewed_revision={})\n{}",
                        custom.manifest.name,
                        amends.reviewed_revision,
                        body.trim()
                    ));
                }
            }
        }
        if chunks.is_empty() {
            Ok(None)
        } else {
            Ok(Some(chunks.join("\n\n")))
        }
    }

    fn read_connector_amendments(&self, entry: &ConnectorEntry) -> Result<Option<String>> {
        let dir = self.paths.connector_amendments_dir();
        let mut chunks = Vec::new();
        let mut candidates = BTreeSet::new();
        candidates.insert(entry.dir_name.clone());
        candidates.insert(entry.manifest.id.clone());
        for cand in candidates {
            let p = dir.join(format!("{cand}.md"));
            if file_exists_no_follow(&p)? {
                let text = read_capped_text(&p, MAX_SKILL_DOC_BYTES, true)?;
                if !text.trim().is_empty() {
                    ensure_no_plaintext_secrets(&text, &self.scrubber)?;
                    chunks.push(text.trim().to_string());
                }
            }
        }
        if entry.bundled {
            for custom in self.connectors.iter().filter(|c| !c.bundled) {
                if let Some(ref amends) = custom.manifest.amends
                    && (amends.connector.eq_ignore_ascii_case(&entry.manifest.id)
                        || amends.connector.eq_ignore_ascii_case(&entry.dir_name))
                {
                    let body = read_capped_text(&custom.doc_path, MAX_SKILL_DOC_BYTES, true)?;
                    ensure_no_plaintext_secrets(&body, &self.scrubber)?;
                    chunks.push(format!(
                        "### Amendment `{}` (reviewed_revision={})\n{}",
                        custom.manifest.id,
                        amends.reviewed_revision,
                        body.trim()
                    ));
                }
            }
        }
        if chunks.is_empty() {
            Ok(None)
        } else {
            Ok(Some(chunks.join("\n\n")))
        }
    }

    fn scan_bundled_skills(&mut self, strict: bool) -> Result<()> {
        let root = self.paths.skills_dir();
        if !root.exists() {
            return Ok(());
        }
        for entry in std::fs::read_dir(&root)? {
            let entry = entry?;
            let path = entry.path();
            let dir_name = entry.file_name().to_string_lossy().to_string();
            if dir_name.starts_with('.') {
                continue;
            }
            if !entry.file_type()?.is_dir() {
                continue;
            }
            if !bundled_name_regex().is_match(&dir_name) {
                continue;
            }
            match load_bundled_skill_entry(&path, &dir_name) {
                Ok(Some(item)) => self.skills.push(item),
                Ok(None) => {}
                Err(err) if strict => return Err(err),
                Err(err) => tracing::warn!("Skipping malformed bundled skill '{dir_name}': {err}"),
            }
        }
        Ok(())
    }

    fn scan_custom_skills(&mut self, strict: bool) -> Result<()> {
        let root = self.paths.custom_skills_dir();
        if !root.exists() {
            return Ok(());
        }
        for entry in std::fs::read_dir(&root)? {
            let entry = entry?;
            let path = entry.path();
            let dir_name = entry.file_name().to_string_lossy().to_string();
            if dir_name.starts_with('.') {
                continue;
            }
            match load_custom_skill_entry(&path, &dir_name, &self.scrubber) {
                Ok(item) => self.skills.push(item),
                Err(err) if strict => return Err(err),
                Err(err) => tracing::warn!("Skipping invalid custom skill '{dir_name}': {err}"),
            }
        }
        Ok(())
    }

    fn scan_bundled_connectors(&mut self, strict: bool) -> Result<()> {
        let root = self.paths.connectors_dir();
        if !root.exists() {
            return Ok(());
        }
        for entry in std::fs::read_dir(&root)? {
            let entry = entry?;
            let path = entry.path();
            let dir_name = entry.file_name().to_string_lossy().to_string();
            if dir_name.starts_with('.') || !entry.file_type()?.is_dir() {
                continue;
            }
            match load_bundled_connector_entry(&path, &dir_name) {
                Ok(Some(item)) => self.connectors.push(item),
                Ok(None) => {}
                Err(err) if strict => return Err(err),
                Err(err) => {
                    tracing::warn!("Skipping malformed bundled connector '{dir_name}': {err}")
                }
            }
        }
        Ok(())
    }

    fn scan_custom_connectors(&mut self, strict: bool) -> Result<()> {
        let root = self.paths.custom_connectors_dir();
        if !root.exists() {
            return Ok(());
        }
        for entry in std::fs::read_dir(&root)? {
            let entry = entry?;
            let path = entry.path();
            let dir_name = entry.file_name().to_string_lossy().to_string();
            if dir_name.starts_with('.') {
                continue;
            }
            match load_custom_connector_entry(&path, &dir_name, &self.scrubber) {
                Ok(item) => self.connectors.push(item),
                Err(err) if strict => return Err(err),
                Err(err) => {
                    tracing::warn!("Skipping invalid custom connector '{dir_name}': {err}")
                }
            }
        }
        Ok(())
    }
}

fn load_bundled_skill_entry(dir: &Path, dir_name: &str) -> Result<Option<SkillEntry>> {
    let yaml_path = dir.join("skill.yaml");
    let doc_path = dir.join("SKILL.md");
    let reference_paths = collect_reference_paths(dir)?;

    if yaml_path.exists() {
        let yaml_raw = read_capped_text(&yaml_path, MAX_SKILL_DOC_BYTES, false)?;
        let bundled: BundledSkillYaml = serde_yaml_ng::from_str(&yaml_raw).map_err(|e| {
            GmError::Config(format!(
                "Failed parsing bundled '{}': {e}",
                yaml_path.display()
            ))
        })?;
        let manifest: SkillYaml = bundled.into();
        let doc = if doc_path.exists() {
            Some(doc_path)
        } else {
            None
        };
        return Ok(Some(SkillEntry {
            dir_name: dir_name.to_string(),
            manifest,
            bundled: true,
            root_dir: dir.to_path_buf(),
            doc_path: doc,
            reference_paths,
        }));
    }

    if reference_paths.is_empty() && !doc_path.exists() {
        return Ok(None);
    }

    let (title, description, category) = match dir_name {
        "artifacts" => (
            "Artifacts",
            "Format guides and references for markdown, documents, presentations, spreadsheets, and PDF deliverables.",
            "Deliverables",
        ),
        "spaces" => (
            "Spaces",
            "Templates and guides for interactive Spaces. Use static Spaces (`kind: static`) under ~/spaces/.",
            "Spaces",
        ),
        _ => (
            dir_name,
            "Bundled Muse reference guide directory.",
            "Reference",
        ),
    };
    Ok(Some(SkillEntry {
        dir_name: dir_name.to_string(),
        manifest: SkillYaml {
            name: dir_name.replace('-', "_"),
            title: title.to_string(),
            description: description.to_string(),
            category: category.to_string(),
            include_in_prompt: false,
            skill_improvement: None,
            amends: None,
        },
        bundled: true,
        root_dir: dir.to_path_buf(),
        doc_path: if doc_path.exists() {
            Some(doc_path)
        } else {
            None
        },
        reference_paths,
    }))
}

fn load_custom_skill_entry(
    dir: &Path,
    dir_name: &str,
    scrubber: &SecretScrubber,
) -> Result<SkillEntry> {
    let meta = std::fs::symlink_metadata(dir)?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(GmError::SafetyViolation(format!(
            "RULE-PATH-002: Custom skill '{dir_name}' must be a regular directory"
        )));
    }
    if !is_valid_custom_skill_name(dir_name) {
        return Err(GmError::Config(format!(
            "Invalid custom skill directory name '{dir_name}': must match ^[a-z0-9][a-z0-9-]{{0,63}}$"
        )));
    }

    let yaml_path = dir.join("skill.yaml");
    let yaml_raw = read_capped_text(&yaml_path, MAX_SKILL_DOC_BYTES, true)?;
    ensure_no_plaintext_secrets(&yaml_raw, scrubber)?;
    let manifest = parse_custom_skill_yaml(&yaml_raw)?;
    if manifest.name != dir_name {
        return Err(GmError::Config(format!(
            "Custom skill manifest name '{}' does not match directory '{dir_name}'",
            manifest.name
        )));
    }

    let doc_path = dir.join("SKILL.md");
    let doc_raw = read_capped_text(&doc_path, MAX_SKILL_DOC_BYTES, true)?;
    ensure_no_plaintext_secrets(&doc_raw, scrubber)?;
    let reference_paths = collect_reference_paths(dir)?;

    Ok(SkillEntry {
        dir_name: dir_name.to_string(),
        manifest,
        bundled: false,
        root_dir: dir.to_path_buf(),
        doc_path: Some(doc_path),
        reference_paths,
    })
}

fn load_bundled_connector_entry(dir: &Path, dir_name: &str) -> Result<Option<ConnectorEntry>> {
    let yaml_path = dir.join("connector.yaml");
    let doc_path = dir.join("CONNECTOR.md");
    if !yaml_path.exists() || !doc_path.exists() {
        return Ok(None);
    }
    let yaml_raw = read_capped_text(&yaml_path, MAX_SKILL_DOC_BYTES, false)?;
    let bundled: BundledConnectorYaml = serde_yaml_ng::from_str(&yaml_raw).map_err(|e| {
        GmError::Config(format!(
            "Failed parsing bundled connector '{}': {e}",
            yaml_path.display()
        ))
    })?;
    let reference_paths = collect_reference_paths(dir)?;
    Ok(Some(ConnectorEntry {
        dir_name: dir_name.to_string(),
        manifest: bundled.into(),
        bundled: true,
        root_dir: dir.to_path_buf(),
        doc_path,
        reference_paths,
    }))
}

fn load_custom_connector_entry(
    dir: &Path,
    dir_name: &str,
    scrubber: &SecretScrubber,
) -> Result<ConnectorEntry> {
    let meta = std::fs::symlink_metadata(dir)?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(GmError::SafetyViolation(format!(
            "RULE-PATH-002: Custom connector '{dir_name}' must be a regular directory"
        )));
    }
    if !is_valid_custom_connector_id(dir_name) {
        return Err(GmError::Config(format!(
            "Invalid custom connector directory '{dir_name}': must match ^custom\\.[a-z0-9-]{{1,63}}$"
        )));
    }

    let yaml_path = dir.join("connector.yaml");
    let yaml_raw = read_capped_text(&yaml_path, MAX_SKILL_DOC_BYTES, true)?;
    ensure_no_plaintext_secrets(&yaml_raw, scrubber)?;
    let manifest = parse_custom_connector_yaml(&yaml_raw)?;
    if manifest.id != dir_name {
        return Err(GmError::Config(format!(
            "Custom connector manifest id '{}' does not match directory '{dir_name}'",
            manifest.id
        )));
    }

    let doc_path = dir.join("CONNECTOR.md");
    let doc_raw = read_capped_text(&doc_path, MAX_SKILL_DOC_BYTES, true)?;
    ensure_no_plaintext_secrets(&doc_raw, scrubber)?;
    let reference_paths = collect_reference_paths(dir)?;

    Ok(ConnectorEntry {
        dir_name: dir_name.to_string(),
        manifest,
        bundled: false,
        root_dir: dir.to_path_buf(),
        doc_path,
        reference_paths,
    })
}

fn file_exists_no_follow(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(GmError::SafetyViolation(format!(
                    "RULE-PATH-002: Refusing symlink '{}'",
                    path.display()
                )));
            }
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

fn read_capped_text(path: &Path, max_bytes: usize, reject_symlinks: bool) -> Result<String> {
    let meta = std::fs::symlink_metadata(path)?;
    if reject_symlinks && meta.file_type().is_symlink() {
        return Err(GmError::SafetyViolation(format!(
            "RULE-PATH-002: Refusing symlink '{}'",
            path.display()
        )));
    }
    if meta.len() > max_bytes as u64 {
        return Err(GmError::Config(format!(
            "Skill/connector file '{}' exceeds {} byte cap ({} bytes)",
            path.display(),
            max_bytes,
            meta.len()
        )));
    }
    let bytes = std::fs::read(path)?;
    if bytes.len() > max_bytes {
        return Err(GmError::Config(format!(
            "Skill/connector file '{}' exceeds {} byte cap ({} bytes)",
            path.display(),
            max_bytes,
            bytes.len()
        )));
    }
    String::from_utf8(bytes)
        .map_err(|_| GmError::Config(format!("File '{}' is not valid UTF-8", path.display())))
}

fn collect_reference_paths(root: &Path) -> Result<Vec<String>> {
    let mut out = Vec::new();
    collect_files_recursive(root, root, &mut out)?;
    out.sort();
    Ok(out)
}

fn collect_files_recursive(base: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let rel = path
            .strip_prefix(base)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        if entry.file_type()?.is_dir() {
            collect_files_recursive(base, &path, out)?;
        } else if rel != "SKILL.md"
            && rel != "skill.yaml"
            && rel != "CONNECTOR.md"
            && rel != "connector.yaml"
        {
            out.push(rel);
        }
    }
    Ok(())
}

fn synthesize_bundle_guide(entry: &SkillEntry) -> String {
    let mut lines = vec![
        format!("# {}", entry.manifest.title),
        String::new(),
        entry.manifest.description.clone(),
        String::new(),
        "## Bundled Guides & References".to_string(),
    ];
    for r in &entry.reference_paths {
        lines.push(format!("- `~/skills/{}/{r}`", entry.dir_name));
    }
    lines.join("\n")
}

fn normalize_key(s: &str) -> String {
    s.trim().to_ascii_lowercase().replace('-', "_")
}

fn is_connector_authenticated(conn: &ConnectorEntry, authenticated: &[String]) -> bool {
    authenticated.iter().any(|id| {
        let q = id.trim();
        conn.manifest.id.eq_ignore_ascii_case(q)
            || conn.dir_name.eq_ignore_ascii_case(q)
            || conn
                .manifest
                .aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(q))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::templates::bootstrap_shared_runtime_files;

    #[test]
    fn bundled_upstream_skills_and_docs_load_cleanly_via_progressive_disclosure() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = MuseRuntimePaths::shared(tmp.path());
        bootstrap_shared_runtime_files(&paths).unwrap();

        let reg = SkillRegistry::open(&paths).unwrap();
        let inv = reg.prompt_inventory(&[]);

        // `include_in_prompt: true` bundled skills appear in prompt inventory without full bodies.
        assert!(inv.contains("`goals`"));
        assert!(inv.contains("`forget`"));
        assert!(inv.contains("`device_data`"));
        assert!(inv.contains("`custom_connector_setup`"));

        // `include_in_prompt: false` skills are omitted from prompt inventory (progressive disclosure)...
        assert!(!inv.contains("`self_awareness`"));
        assert!(!inv.contains("`wide_research`"));
        // ...but remain searchable and loadable on demand with references!
        let hits = reg.search_skills("self-referential");
        assert!(hits.iter().any(|e| e.manifest.name == "self_awareness"));

        let sa = reg.load_skill("self-awareness").unwrap();
        assert!(sa.skill_markdown.contains('#') || !sa.skill_markdown.is_empty());
        assert!(
            sa.references
                .iter()
                .any(|r| r == "references/question_types.md")
        );

        let goals = reg.load_skill("goals").unwrap();
        assert!(goals.references.iter().any(|r| r.starts_with("creation/")));
        assert!(goals.references.iter().any(|r| r.starts_with("guides/")));

        let forget = reg.load_skill("forget").unwrap();
        assert!(
            forget
                .references
                .iter()
                .any(|r| r == "references/artifact-inventory.md")
        );

        let creator = reg.load_skill("custom_connector_setup").unwrap();
        assert!(
            creator
                .references
                .iter()
                .any(|r| r == "references/amendments.md")
        );
        assert!(
            creator
                .references
                .iter()
                .any(|r| r.starts_with("templates/"))
        );

        let artifacts = reg.load_skill("artifacts").unwrap();
        assert!(artifacts.references.iter().any(|r| r == "pdf/GUIDE.md"));
        assert!(
            artifacts
                .references
                .iter()
                .any(|r| r == "markdown/GUIDE.md")
        );

        let spaces = reg.load_skill("spaces").unwrap();
        assert!(spaces.skill_markdown.contains("TypeScript Spaces"));
        assert!(
            spaces
                .references
                .iter()
                .any(|r| r.starts_with("templates/space-static/"))
        );

        for doc in [
            "muse.md",
            "data-handling.md",
            "client-surfaces.md",
            "goals.md",
            "feed.md",
            "ideas.md",
            "memory.md",
            "privacy-and-credentials.md",
            "artifacts.md",
            "scheduling-and-watching.md",
            "self_improvement.md",
            "calls-texts-notifications.md",
            "connectors.md",
        ] {
            assert!(
                paths.docs_dir().join(doc).exists(),
                "Materialized upstream doc missing: {doc}"
            );
        }
    }

    #[test]
    fn require_auth_hides_unauthenticated_connectors_and_search_matches_aliases() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = MuseRuntimePaths::shared(tmp.path());
        bootstrap_shared_runtime_files(&paths).unwrap();

        let mut reg = SkillRegistry::open(&paths).unwrap();
        reg.scaffold_custom_connector(
            ConnectorYaml {
                id: "custom.linear".into(),
                title: "Linear".into(),
                description: "Manage engineering issues in Linear.".into(),
                include_in_prompt: true,
                require_auth: true,
                aliases: vec!["Issue Tracker".into(), "LN".into()],
                amends: None,
            },
            "# custom.linear\nUse secret://keychain/linear for auth.\n",
        )
        .unwrap();
        reg.scaffold_custom_connector(
            ConnectorYaml {
                id: "custom.weather".into(),
                title: "OpenMeteo".into(),
                description: "Public forecast API without authentication.".into(),
                include_in_prompt: true,
                require_auth: false,
                aliases: vec!["Weather Service".into()],
                amends: None,
            },
            "# custom.weather\nPublic endpoint.\n",
        )
        .unwrap();

        let unauth_inv = reg.prompt_inventory(&[]);
        assert!(!unauth_inv.contains("custom.linear"));
        assert!(unauth_inv.contains("custom.weather"));

        let authed_inv = reg.prompt_inventory(&["custom.linear".to_string()]);
        assert!(authed_inv.contains("custom.linear"));
        assert!(authed_inv.contains("custom.weather"));

        assert_eq!(reg.search_connectors("Issue Tracker").len(), 1);
        let loaded = reg.load_connector("LN").unwrap();
        assert_eq!(loaded.manifest.id, "custom.linear");
        assert!(
            loaded
                .connector_markdown
                .contains("secret://keychain/linear")
        );
    }

    #[test]
    fn amendment_overlay_applies_to_system_and_custom_skills() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = MuseRuntimePaths::shared(tmp.path());
        bootstrap_shared_runtime_files(&paths).unwrap();

        let reg = SkillRegistry::open(&paths).unwrap();
        reg.append_amendment(
            AmendmentTarget::Skill,
            "goals",
            "Always link new subgoals under the parent GOAL.md checklist.",
        )
        .unwrap();
        reg.append_amendment(
            AmendmentTarget::Skill,
            "goals",
            "Keep milestone dates in ISO-8601 format.",
        )
        .unwrap();

        let loaded = reg.load_skill("goals").unwrap();
        let amendment = loaded.amendment_markdown.expect("amendment present");
        assert!(amendment.contains("Always link new subgoals"));
        assert!(amendment.contains("Keep milestone dates"));
    }

    #[test]
    fn rejects_plaintext_secrets_bad_names_oversize_files_and_unknown_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = MuseRuntimePaths::shared(tmp.path());
        bootstrap_shared_runtime_files(&paths).unwrap();

        let scrubber = SecretScrubber::new();
        scrubber.register(
            "secret://keychain/my_api",
            "super_registered_live_secret_987",
        );
        let mut reg = SkillRegistry::open_with_scrubber(&paths, scrubber).unwrap();

        // Plaintext secret patterns and registered vault values are refused with guidance.
        let secret_err = reg
            .scaffold_custom_skill(
                SkillYaml {
                    name: "bad-secret-skill".into(),
                    title: "Bad Secret".into(),
                    description: "Attempts to store a raw key.".into(),
                    category: "Test".into(),
                    include_in_prompt: false,
                    skill_improvement: None,
                    amends: None,
                },
                "TOKEN=ghp_1234567890abcdefghijklmnop",
            )
            .unwrap_err()
            .to_string();
        assert!(secret_err.contains("secret://keychain/<provider>"));

        let registered_err = reg
            .append_amendment(
                AmendmentTarget::Skill,
                "goals",
                "Use key super_registered_live_secret_987 here.",
            )
            .unwrap_err()
            .to_string();
        assert!(registered_err.contains("secret://keychain/<provider>"));

        // Safe reference scheme is accepted.
        reg.scaffold_custom_skill(
            SkillYaml {
                name: "vault-ref-skill".into(),
                title: "Vault Ref".into(),
                description: "Uses opaque vault ref.".into(),
                category: "Test".into(),
                include_in_prompt: false,
                skill_improvement: None,
                amends: None,
            },
            "Resolve via secret://keychain/my_api or secret://1password/Dev/Token.",
        )
        .unwrap();

        // Bad skill names and bad connector ids rejected.
        for bad_name in ["BadUppercase", "under_score", "../escape", ""] {
            assert!(
                reg.scaffold_custom_skill(
                    SkillYaml {
                        name: bad_name.into(),
                        title: "Bad".into(),
                        description: "Bad".into(),
                        category: "Test".into(),
                        include_in_prompt: false,
                        skill_improvement: None,
                        amends: None,
                    },
                    "# Body",
                )
                .is_err(),
                "Expected bad name '{bad_name}' to be rejected"
            );
        }
        for bad_id in ["linear", "custom.Bad_Id", "custom."] {
            assert!(
                reg.scaffold_custom_connector(
                    ConnectorYaml {
                        id: bad_id.into(),
                        title: "Bad".into(),
                        description: "Bad".into(),
                        include_in_prompt: false,
                        require_auth: true,
                        aliases: vec![],
                        amends: None,
                    },
                    "# Doc",
                )
                .is_err(),
                "Expected bad connector id '{bad_id}' to be rejected"
            );
        }

        // Oversize (> 256 KiB) rejected.
        let huge = "x".repeat(MAX_SKILL_DOC_BYTES + 1);
        assert!(
            reg.scaffold_custom_skill(
                SkillYaml {
                    name: "oversize-skill".into(),
                    title: "Big".into(),
                    description: "Too large".into(),
                    category: "Test".into(),
                    include_in_prompt: false,
                    skill_improvement: None,
                    amends: None,
                },
                &huge,
            )
            .is_err()
        );

        // Unknown YAML field on user-authored skill.yaml rejected (deny_unknown_fields).
        let custom_dir = paths.custom_skills_dir().join("unknown-field-skill");
        ensure_private_dir(&custom_dir).unwrap();
        std::fs::write(
            custom_dir.join("skill.yaml"),
            "name: unknown-field-skill\ntitle: X\ndescription: Y\ncategory: Z\nextra_field: 1\n",
        )
        .unwrap();
        std::fs::write(custom_dir.join("SKILL.md"), "# Body\n").unwrap();
        assert!(SkillRegistry::open(&paths).is_err());
    }
}
