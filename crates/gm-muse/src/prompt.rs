//! Unified Muse system prompt builder combining verbatim upstream baseline,
//! Grok-Muse Runtime Adaptation, per-bot persona files, fenced user/memory
//! data, progressive skills inventory, and dynamic timezone/model tail.

use crate::templates::read_asset_str;
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use gm_core::config::MuseConfig;
use gm_core::error::Result;
use gm_core::muse_paths::MuseRuntimePaths;
use gm_core::storage::atomic_write_private;
use gm_llm::ChatMessage;
use gm_safety::{SecretScrubber, format_data_fence};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Surfaces on which a Muse prompt can be constructed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptSurface {
    MainChat,
    SideChat,
    OpenBotTask,
    Heartbeat,
    Subagent,
}

/// Prompt sizing tier mapped from [`PromptSurface`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptTier {
    Full,
    Compact,
    Minimal,
}

impl PromptSurface {
    pub fn tier(self) -> PromptTier {
        match self {
            Self::MainChat | Self::SideChat => PromptTier::Full,
            Self::OpenBotTask | Self::Subagent => PromptTier::Compact,
            Self::Heartbeat => PromptTier::Minimal,
        }
    }
}

impl PromptTier {
    pub const fn baseline_cap(self) -> usize {
        match self {
            Self::Full => 80_000,
            Self::Compact => 24_000,
            Self::Minimal => 6_000,
        }
    }

    pub const fn persona_cap(self) -> usize {
        match self {
            Self::Full => 12_000,
            Self::Compact => 6_000,
            Self::Minimal => 3_000,
        }
    }

    pub const fn data_cap(self) -> usize {
        match self {
            Self::Full => 16_000,
            Self::Compact => 8_000,
            Self::Minimal => 3_000,
        }
    }

    pub const fn skills_cap(self) -> usize {
        match self {
            Self::Full => 8_000,
            Self::Compact => 4_000,
            Self::Minimal => 0,
        }
    }

    pub const fn total_cap(self) -> usize {
        match self {
            Self::Full => 140_000,
            Self::Compact => 56_000,
            Self::Minimal => 16_000,
        }
    }
}

/// Upstream `system_prompt.md` headings selected verbatim for [`PromptTier::Compact`] and
/// [`PromptTier::Minimal`], covering identity, truth/values, discretion, untrusted content
/// prompt-injection resistance, external parties, writing style, security, anti-phishing,
/// and hard safety boundaries.
pub const COMPACT_SECTIONS: &[&str] = &[
    "## Who You Are",
    "### Truth",
    "### Who Built You",
    "### Who You Work For",
    "### Discretion and Alignment",
    "### External Parties",
    "## Writing Style",
    "## Security",
    "### Anti-Phishing",
    "## Safety",
];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserTimezones {
    #[serde(default)]
    pub current_tz: Option<String>,
    #[serde(default)]
    pub home_tz: Option<String>,
    #[serde(default)]
    pub work_tz: Option<String>,
}

/// All runtime inputs needed to render the unified Muse system prompt.
#[derive(Debug, Clone)]
pub struct PromptInput<'a> {
    pub surface: PromptSurface,
    pub paths: &'a MuseRuntimePaths,
    pub config: &'a MuseConfig,
    pub model_display_name: &'a str,
    pub skills_inventory: Option<&'a str>,
    pub authenticated_connectors: &'a [String],
    pub goals_summary: Option<&'a str>,
    pub channel_instructions: Option<&'a str>,
    pub allow_host_desktop: bool,
    pub workspace_vm_enabled: bool,
    pub now: DateTime<Utc>,
    pub timezone_override: Option<&'a str>,
}

impl<'a> PromptInput<'a> {
    pub fn new(
        surface: PromptSurface,
        paths: &'a MuseRuntimePaths,
        config: &'a MuseConfig,
        model_display_name: &'a str,
    ) -> Self {
        Self {
            surface,
            paths,
            config,
            model_display_name,
            skills_inventory: None,
            authenticated_connectors: &[],
            goals_summary: None,
            channel_instructions: None,
            allow_host_desktop: false,
            workspace_vm_enabled: false,
            now: Utc::now(),
            timezone_override: None,
        }
    }
}

/// Rendered two-message system prompt output plus byte accounting metadata.
#[derive(Debug, Clone)]
pub struct RenderedPrompt {
    pub messages: Vec<ChatMessage>,
    pub tier: PromptTier,
    pub effective_timezone: String,
    pub total_bytes: usize,
}

/// Stateless prompt builder turning runtime files + upstream baseline into
/// cache-friendly `[stable_prefix, dynamic_tail]` `system` `ChatMessage`s.
pub struct MusePromptBuilder;

impl MusePromptBuilder {
    pub fn build(input: &PromptInput<'_>) -> Result<RenderedPrompt> {
        let tier = input.surface.tier();
        let scrubber = SecretScrubber::new();

        // 1. Dynamic tail message (placed last so the stable prefix stays prompt-cacheable)
        let (tz, effective_tz_name) =
            resolve_effective_timezone(input.paths, input.config, input.timezone_override);
        let local_dt = input.now.with_timezone(&tz);
        let model_name = if input.model_display_name.trim().is_empty() {
            "unknown-model"
        } else {
            input.model_display_name.trim()
        };
        let dynamic_msg_content = format!(
            "[{}] [client_timezone={}] [model={}]",
            local_dt.format("%a %Y-%m-%d %H:%M:%S %Z"),
            effective_tz_name,
            model_name
        );

        let max_prefix_bytes = tier.total_cap().saturating_sub(dynamic_msg_content.len());

        // 2. Verbatim upstream baseline
        let upstream_raw = read_asset_str("upstream/system_prompt.md")?;
        let baseline = match tier {
            PromptTier::Full => {
                truncate_utf8_for_file(upstream_raw, tier.baseline_cap(), "system_prompt.md")
            }
            PromptTier::Compact | PromptTier::Minimal => {
                let selected = extract_sections_by_heading(upstream_raw, COMPACT_SECTIONS);
                truncate_utf8_for_file(&selected, tier.baseline_cap(), "system_prompt.md")
            }
        };

        // 3. Original Grok-Muse Runtime Adaptation section
        let adaptation = render_runtime_adaptation(
            model_name,
            input.config,
            input.allow_host_desktop,
            input.workspace_vm_enabled,
        )?;

        let mut prefix = String::with_capacity(baseline.len() + adaptation.len() + 4096);
        append_capped_plain(
            &mut prefix,
            max_prefix_bytes,
            &baseline,
            "workspace/system/system_prompt.md",
        );
        append_capped_plain(
            &mut prefix,
            max_prefix_bytes,
            &adaptation,
            "workspace/system/system_prompt.md",
        );

        // 4. Per-bot persona files (`SOUL.md`, `IDENTITY.md`, `AGENTS.md`, `TOOLS.md`)
        for (maybe_path, heading, virtual_file) in [
            (input.paths.soul_md(), "## Persona (SOUL.md)", "SOUL.md"),
            (
                input.paths.identity_md(),
                "## Identity (IDENTITY.md)",
                "IDENTITY.md",
            ),
            (
                input.paths.agents_md(),
                "## Workspace Rules (AGENTS.md)",
                "AGENTS.md",
            ),
            (
                input.paths.tools_md(),
                "## Tool Notes (TOOLS.md)",
                "TOOLS.md",
            ),
        ] {
            if let Some(path) = maybe_path
                && let Some(raw) = read_optional_trimmed(&path)
            {
                let clean = scrubber.scrub_str(&raw);
                let capped = truncate_utf8_for_file(&clean, tier.persona_cap(), virtual_file);
                let section = format!("{heading}\n{capped}");
                append_capped_plain(&mut prefix, max_prefix_bytes, &section, virtual_file);
            }
        }

        // 5. Progressive-disclosure skills inventory (omitted for Minimal/Heartbeat)
        if tier.skills_cap() > 0 {
            let auto_inventory;
            let resolved_inventory = match input
                .skills_inventory
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                Some(explicit) => Some(explicit),
                None if input.config.enable_skills => {
                    let reg =
                        crate::skills::SkillRegistry::open_resilient(input.paths, scrubber.clone());
                    let inv = reg.prompt_inventory(input.authenticated_connectors);
                    if inv.trim().is_empty() {
                        None
                    } else {
                        auto_inventory = inv;
                        Some(auto_inventory.as_str())
                    }
                }
                None => None,
            };
            if let Some(inv) = resolved_inventory {
                let clean = scrubber.scrub_str(inv);
                let capped = truncate_utf8_for_file(&clean, tier.skills_cap(), "skills/index");
                let section = format!("## Available Skills & Connectors\n{capped}");
                append_capped_plain(&mut prefix, max_prefix_bytes, &section, "skills/index");
            }
        }

        // 6. Fenced user/memory/preferences/relationship/alignment data files
        if let Some(user_raw) = read_optional_trimmed(&input.paths.user_md()) {
            append_capped_fenced(
                &mut prefix,
                max_prefix_bytes,
                tier.data_cap(),
                "User Profile",
                "user_profile_data",
                &scrubber.scrub_str(&user_raw),
                "USER.md",
            );
        }

        if let Some(mem_raw) = read_optional_trimmed(&input.paths.memory_md()) {
            append_capped_fenced(
                &mut prefix,
                max_prefix_bytes,
                tier.data_cap(),
                "Saved Long-Term Memory",
                "memory_data",
                &scrubber.scrub_str(&mem_raw),
                "MEMORY.md",
            );
        }

        if let Some(prefs_raw) = read_optional_trimmed(&input.paths.proactive_preferences_md()) {
            append_capped_fenced(
                &mut prefix,
                max_prefix_bytes,
                tier.data_cap(),
                "Proactive Preferences",
                "proactive_preferences_data",
                &scrubber.scrub_str(&prefs_raw),
                "PROACTIVE_PREFERENCES.md",
            );
        }

        if let Some(rel_raw) = build_relationship_indexes_text(input.paths) {
            append_capped_fenced(
                &mut prefix,
                max_prefix_bytes,
                tier.data_cap(),
                "Relationship Indexes",
                "relationship_indexes_data",
                &scrubber.scrub_str(&rel_raw),
                "memory/people/INDEX.md",
            );
        }

        if let Some(align_raw) = read_optional_trimmed(&input.paths.alignment_synthesis_md()) {
            append_capped_fenced(
                &mut prefix,
                max_prefix_bytes,
                tier.data_cap(),
                "Alignment Synthesis",
                "alignment_synthesis_data",
                &scrubber.scrub_str(&align_raw),
                "dreams/alignment/derived/ALIGNMENT_SYNTHESIS.md",
            );
        }

        // 7. Goals summary
        if let Some(goals) = input.goals_summary.map(str::trim).filter(|s| !s.is_empty()) {
            let clean = scrubber.scrub_str(goals);
            let capped = truncate_utf8_for_file(&clean, tier.data_cap(), "workspace/goals");
            let section = format!("## Active Goals\n{capped}");
            append_capped_plain(&mut prefix, max_prefix_bytes, &section, "workspace/goals");
        }

        // 8. Channel instructions
        if let Some(channel_inst) = input
            .channel_instructions
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let clean = scrubber.scrub_str(channel_inst);
            let capped = truncate_utf8_for_file(&clean, tier.data_cap(), "channels");
            let section = format!("## Channel Context\n{capped}");
            append_capped_plain(&mut prefix, max_prefix_bytes, &section, "channels");
        }

        // Final hard invariant: rendered system messages never exceed `tier.total_cap()`.
        if prefix.len() > max_prefix_bytes {
            prefix = truncate_utf8_for_file(
                &prefix,
                max_prefix_bytes,
                "workspace/system/system_prompt.md",
            );
        }

        let total_bytes = prefix.len() + dynamic_msg_content.len();
        let messages = vec![
            ChatMessage::text("system", prefix),
            ChatMessage::text("system", dynamic_msg_content),
        ];

        Ok(RenderedPrompt {
            messages,
            tier,
            effective_timezone: effective_tz_name,
            total_bytes,
        })
    }

    pub fn build_messages(input: &PromptInput<'_>) -> Result<Vec<ChatMessage>> {
        Ok(Self::build(input)?.messages)
    }
}

/// Write the rendered MainChat prompt to `~/workspace/system/system_prompt.md` at daemon start.
pub fn write_main_chat_system_prompt(
    paths: &MuseRuntimePaths,
    config: &MuseConfig,
    model_display_name: &str,
    allow_host_desktop: bool,
    workspace_vm_enabled: bool,
    skills_inventory: Option<&str>,
) -> Result<PathBuf> {
    let mut input = PromptInput::new(PromptSurface::MainChat, paths, config, model_display_name);
    input.allow_host_desktop = allow_host_desktop;
    input.workspace_vm_enabled = workspace_vm_enabled;
    input.skills_inventory = skills_inventory;
    let rendered = MusePromptBuilder::build(&input)?;
    let combined = rendered
        .messages
        .iter()
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    let dest = paths.system_prompt_md();
    atomic_write_private(&dest, combined.as_bytes())?;
    Ok(dest)
}

/// Truncate `s` to at most `max_bytes` cleanly on a UTF-8 char boundary, appending a
/// `WorkspaceReadFile` continuation notice when omitted bytes remain and fit within `max_bytes`.
pub fn truncate_utf8(s: &str, max_bytes: usize) -> String {
    truncate_utf8_for_file(s, max_bytes, "workspace/system/system_prompt.md")
}

/// Truncate `s` to at most `max_bytes` cleanly on a UTF-8 char boundary, referencing
/// `~/<virtual_file>` in the continuation hint.
pub fn truncate_utf8_for_file(s: &str, max_bytes: usize, virtual_file: &str) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    if max_bytes == 0 {
        return String::new();
    }

    let clean_file = virtual_file.trim_start_matches("~/");
    let max_notice_overhead = format!(
        "\n[…truncated {} bytes — read ~/{} with WorkspaceReadFile for the rest]",
        s.len(),
        clean_file
    )
    .len();

    if max_bytes <= max_notice_overhead {
        let mut cut = max_bytes.min(s.len());
        while cut > 0 && !s.is_char_boundary(cut) {
            cut -= 1;
        }
        return s[..cut].to_string();
    }

    let mut cut = max_bytes - max_notice_overhead;
    loop {
        while cut > 0 && !s.is_char_boundary(cut) {
            cut -= 1;
        }
        let omitted = s.len().saturating_sub(cut);
        let notice = format!(
            "\n[…truncated {omitted} bytes — read ~/{clean_file} with WorkspaceReadFile for the rest]"
        );
        if cut + notice.len() <= max_bytes {
            let mut out = String::with_capacity(cut + notice.len());
            out.push_str(&s[..cut]);
            out.push_str(&notice);
            return out;
        }
        if cut == 0 {
            return String::new();
        }
        cut -= 1;
    }
}

/// Extract sections verbatim from markdown whose exact heading lines match `wanted_headings`.
pub fn extract_sections_by_heading(markdown: &str, wanted_headings: &[&str]) -> String {
    let mut out = Vec::new();
    let mut include_current = false;
    let mut current_buf = String::new();

    for line in markdown.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(&['\r', '\n'][..]);
        if trimmed.starts_with("## ") || trimmed.starts_with("### ") {
            if include_current && !current_buf.is_empty() {
                out.push(current_buf.trim_end_matches('\n').to_string());
                current_buf.clear();
            }
            include_current = wanted_headings.contains(&trimmed);
            if include_current {
                current_buf.push_str(line);
            }
        } else if include_current {
            current_buf.push_str(line);
        }
    }

    if include_current && !current_buf.is_empty() {
        out.push(current_buf.trim_end_matches('\n').to_string());
    }

    out.join("\n\n")
}

/// Resolve timezone from `override`, else `~/workspace/user/timezones.yaml`, else
/// `config.default_timezone`; invalid IANA names log a warning and fall back to UTC.
pub fn resolve_effective_timezone(
    paths: &MuseRuntimePaths,
    config: &MuseConfig,
    override_tz: Option<&str>,
) -> (Tz, String) {
    let file_tz = read_timezones_yaml(&paths.timezones_yaml());
    let candidate = override_tz
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| file_tz.current_tz.filter(|s| !s.trim().is_empty()))
        .or_else(|| file_tz.home_tz.filter(|s| !s.trim().is_empty()))
        .or_else(|| file_tz.work_tz.filter(|s| !s.trim().is_empty()))
        .unwrap_or_else(|| config.default_timezone.clone());

    let trimmed = candidate.trim();
    match trimmed.parse::<Tz>() {
        Ok(tz) => (tz, tz.name().to_string()),
        Err(_) => {
            tracing::warn!(
                timezone = %trimmed,
                "Invalid IANA timezone; falling back to UTC"
            );
            (chrono_tz::UTC, "UTC".to_string())
        }
    }
}

fn read_timezones_yaml(path: &Path) -> UserTimezones {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return UserTimezones::default();
    };
    serde_yaml_ng::from_str(&raw).unwrap_or_default()
}

fn render_runtime_adaptation(
    model_display_name: &str,
    config: &MuseConfig,
    allow_host_desktop: bool,
    workspace_vm_enabled: bool,
) -> Result<String> {
    let template = read_asset_str("grok_muse/runtime_adaptation.md")?;
    let mut unavailable = vec![
        "- `/opt/hatch/bin/remote-storage` & `JARVIS_SANDBOX_API_SOCK`: not available locally (use `sandbox://outbox/<file>` or `~/workspace/goals/<slug>/files/` and standard step actions).".to_string(),
        "- TypeScript Spaces (`kind: ts`): not supported yet (only static HTML/JS/CSS Spaces on the dedicated local Spaces port are available).".to_string(),
    ];
    if !allow_host_desktop {
        unavailable.push(
            "- Host desktop GUI automation & camera feeds: disabled (`safety.allow_host_desktop = false`).".to_string(),
        );
    }
    if !workspace_vm_enabled {
        unavailable.push(
            "- Offline WorkspaceShell Linux VM: unavailable (`workspace_vm.enabled = false`)."
                .to_string(),
        );
    }
    if !config.enable_spaces {
        unavailable
            .push("- Static Spaces web viewer: disabled (`muse.enable_spaces = false`).".into());
    }
    if !config.enable_heartbeat {
        unavailable.push(
            "- Background Heartbeat worker: disabled (`muse.enable_heartbeat = false`).".into(),
        );
    }
    if !config.enable_dreams {
        unavailable.push(
            "- Nightly Dreams & self-improvement consolidation: disabled (`muse.enable_dreams = false`).".into(),
        );
    }
    unavailable.push(
        "- Native telephony calls/SMS & Wallet checkout cards: available only when configured via an explicit MCP connector.".to_string(),
    );

    Ok(template
        .replace("{{MODEL_DISPLAY_NAME}}", model_display_name)
        .replace("{{UNAVAILABLE_SECTION}}", &unavailable.join("\n")))
}

fn read_optional_trimmed(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let trimmed = content.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn build_relationship_indexes_text(paths: &MuseRuntimePaths) -> Option<String> {
    let people = read_optional_trimmed(&paths.people_index_md());
    let groups = read_optional_trimmed(&paths.groups_index_md());
    match (people, groups) {
        (None, None) => None,
        (Some(p), None) => Some(p),
        (None, Some(g)) => Some(g),
        (Some(p), Some(g)) => Some(format!("{p}\n\n{g}")),
    }
}

fn append_capped_plain(prefix: &mut String, max_prefix_bytes: usize, section: &str, file: &str) {
    if section.is_empty() || prefix.len() >= max_prefix_bytes {
        return;
    }
    let sep = if prefix.is_empty() { "" } else { "\n\n" };
    let remaining = max_prefix_bytes.saturating_sub(prefix.len() + sep.len());
    if remaining < 32 {
        return;
    }
    prefix.push_str(sep);
    if section.len() <= remaining {
        prefix.push_str(section);
    } else {
        prefix.push_str(&truncate_utf8_for_file(section, remaining, file));
    }
}

fn append_capped_fenced(
    prefix: &mut String,
    max_prefix_bytes: usize,
    data_cap: usize,
    label: &str,
    tag: &str,
    raw_content: &str,
    virtual_file: &str,
) {
    if raw_content.is_empty() || prefix.len() >= max_prefix_bytes {
        return;
    }
    let sep = if prefix.is_empty() { "" } else { "\n\n" };
    let header = format!("### {label} (~/{virtual_file})\n");
    let wrapper_overhead = header.len() + (2 * tag.len()) + 7; // "<tag>\n...\n</tag>"
    let remaining = max_prefix_bytes.saturating_sub(prefix.len() + sep.len());
    let max_body = data_cap.min(remaining).saturating_sub(wrapper_overhead);
    if max_body < 32 {
        return;
    }
    let body = truncate_utf8_for_file(raw_content, max_body, virtual_file);
    let fenced = format_data_fence(tag, &body);
    let block = format!("{header}{fenced}");
    if prefix.len() + sep.len() + block.len() <= max_prefix_bytes {
        prefix.push_str(sep);
        prefix.push_str(&block);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::templates::{BotSeed, bootstrap_bot_runtime_files, bootstrap_shared_runtime_files};
    use gm_core::config::MuseConfig;

    #[test]
    fn compact_sections_all_exist_in_upstream_system_prompt() {
        let upstream = read_asset_str("upstream/system_prompt.md").unwrap();
        let headings: Vec<&str> = upstream
            .lines()
            .filter(|l| l.starts_with("## ") || l.starts_with("### "))
            .collect();
        for expected in COMPACT_SECTIONS {
            assert!(
                headings.contains(expected),
                "Heading '{expected}' in COMPACT_SECTIONS missing from upstream/system_prompt.md"
            );
        }
        let compact_text = extract_sections_by_heading(upstream, COMPACT_SECTIONS);
        assert!(
            compact_text.len() <= PromptTier::Compact.baseline_cap(),
            "Compact baseline {} exceeds cap {}",
            compact_text.len(),
            PromptTier::Compact.baseline_cap()
        );
        assert!(compact_text.contains("[BEGIN EXTERNAL CONTENT]"));
        assert!(compact_text.contains("### Anti-Phishing"));
        assert!(compact_text.contains("## Safety"));
    }

    #[test]
    fn full_output_contains_verbatim_baseline_and_respects_all_tier_caps() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = tmp.path().join("storage");
        let bot_root = storage.join("bots/muse");
        let paths = MuseRuntimePaths::for_bot(&storage, &bot_root, "muse");
        let seed = BotSeed {
            id: "muse",
            display_name: "Muse Operator",
            role_title: "Lead Operator",
            legacy_system_prompt: "",
        };
        bootstrap_shared_runtime_files(&paths).unwrap();
        bootstrap_bot_runtime_files(&paths, &seed).unwrap();

        // Write huge files to stress per-section and total caps across every tier.
        std::fs::write(paths.memory_md(), "Memory fact.\n".repeat(4_000)).unwrap();
        std::fs::write(paths.soul_md().unwrap(), "Soul line.\n".repeat(4_000)).unwrap();
        std::fs::write(paths.user_md(), "User fact.\n".repeat(4_000)).unwrap();

        let cfg = MuseConfig::default();
        let fixed_now = DateTime::parse_from_rfc3339("2026-11-15T01:25:06Z")
            .unwrap()
            .with_timezone(&Utc);
        let upstream_baseline = read_asset_str("upstream/system_prompt.md").unwrap();

        let big_skills = "Skill list entry\n".repeat(1_000);
        let big_goals = "Goal entry\n".repeat(500);
        let big_channel = "Channel rule\n".repeat(500);

        for surface in [
            PromptSurface::MainChat,
            PromptSurface::SideChat,
            PromptSurface::OpenBotTask,
            PromptSurface::Subagent,
            PromptSurface::Heartbeat,
        ] {
            let mut input = PromptInput::new(surface, &paths, &cfg, "muse-spark-1.3");
            input.now = fixed_now;
            input.skills_inventory = Some(&big_skills);
            input.goals_summary = Some(&big_goals);
            input.channel_instructions = Some(&big_channel);

            let rendered = MusePromptBuilder::build(&input).unwrap();
            let tier = surface.tier();
            assert!(
                rendered.total_bytes <= tier.total_cap(),
                "Surface {:?} exceeded total cap: {} > {}",
                surface,
                rendered.total_bytes,
                tier.total_cap()
            );
            assert_eq!(rendered.messages.len(), 2);
            assert_eq!(rendered.messages[0].role, "system");
            assert_eq!(rendered.messages[1].role, "system");
            assert!(
                rendered.messages[1]
                    .content
                    .contains("[model=muse-spark-1.3]")
            );

            if tier == PromptTier::Full {
                assert!(
                    rendered.messages[0].content.contains(upstream_baseline),
                    "Full prompt must contain verbatim upstream baseline byte-for-byte"
                );
            }
            if tier == PromptTier::Minimal {
                assert!(!rendered.messages[0].content.contains("## Available Skills"));
            }
        }

        let written_path = write_main_chat_system_prompt(
            &paths,
            &cfg,
            "muse-spark-1.3",
            true,
            false,
            Some("- `goals`"),
        )
        .unwrap();
        assert_eq!(written_path, paths.system_prompt_md());
        assert!(
            std::fs::read_to_string(&written_path)
                .unwrap()
                .contains("## Runtime Adaptation")
        );
    }

    #[test]
    fn emoji_at_truncation_boundary_never_panics_and_fits_cap() {
        let mut s = "a".repeat(95);
        s.push_str("🦀🦀🦀🦀🦀");
        s.push_str(&"b".repeat(100));

        for max in [0, 1, 2, 5, 96, 97, 98, 99, 120, 150] {
            let truncated = truncate_utf8_for_file(&s, max, "MEMORY.md");
            assert!(
                truncated.len() <= max,
                "max={max} yielded len={}",
                truncated.len()
            );
            assert!(std::str::from_utf8(truncated.as_bytes()).is_ok());
        }

        let wide = truncate_utf8_for_file(&"🦀".repeat(500), 200, "MEMORY.md");
        assert!(wide.len() <= 200);
        assert!(wide.contains("[…truncated "));
        assert!(wide.contains("read ~/MEMORY.md with WorkspaceReadFile for the rest]"));
    }

    #[test]
    fn timezone_dst_transition_and_invalid_timezone_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = tmp.path().join("storage");
        gm_core::storage::ensure_private_dir(&storage.join("workspace/user")).unwrap();
        let paths = MuseRuntimePaths::shared(&storage);
        std::fs::write(
            paths.timezones_yaml(),
            "home_tz: America/Los_Angeles\nwork_tz: America/New_York\n",
        )
        .unwrap();

        let cfg = MuseConfig::default();

        // Standard time (November)
        let mut winter_in = PromptInput::new(PromptSurface::MainChat, &paths, &cfg, "grok-4");
        winter_in.now = DateTime::parse_from_rfc3339("2026-11-15T01:25:06Z")
            .unwrap()
            .with_timezone(&Utc);
        let winter = MusePromptBuilder::build(&winter_in).unwrap();
        assert_eq!(
            winter.messages[1].content,
            "[Sat 2026-11-14 17:25:06 PST] [client_timezone=America/Los_Angeles] [model=grok-4]"
        );

        // Daylight saving time (June)
        let mut summer_in = PromptInput::new(PromptSurface::MainChat, &paths, &cfg, "grok-4");
        summer_in.now = DateTime::parse_from_rfc3339("2026-06-15T00:25:06Z")
            .unwrap()
            .with_timezone(&Utc);
        let summer = MusePromptBuilder::build(&summer_in).unwrap();
        assert_eq!(
            summer.messages[1].content,
            "[Sun 2026-06-14 17:25:06 PDT] [client_timezone=America/Los_Angeles] [model=grok-4]"
        );

        // Invalid timezone falls back cleanly to UTC
        let bad_cfg = MuseConfig {
            default_timezone: "Mars/Olympus_Mons".into(),
            ..Default::default()
        };
        std::fs::remove_file(paths.timezones_yaml()).unwrap();
        let mut invalid_in =
            PromptInput::new(PromptSurface::MainChat, &paths, &bad_cfg, "muse-spark");
        invalid_in.now = winter_in.now;
        let fallback = MusePromptBuilder::build(&invalid_in).unwrap();
        assert_eq!(fallback.effective_timezone, "UTC");
        assert!(
            fallback.messages[1]
                .content
                .contains("[client_timezone=UTC]")
        );
    }

    #[test]
    fn fixed_clock_main_chat_snapshot_matches_or_updates() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = tmp.path().join("storage");
        let bot_root = storage.join("bots/muse");
        let paths = MuseRuntimePaths::for_bot(&storage, &bot_root, "muse");
        let seed = BotSeed {
            id: "muse",
            display_name: "Muse Operator",
            role_title: "General Desktop Operator & Coordinator",
            legacy_system_prompt: "",
        };
        bootstrap_shared_runtime_files(&paths).unwrap();
        bootstrap_bot_runtime_files(&paths, &seed).unwrap();

        let cfg = MuseConfig::default();
        let mut input = PromptInput::new(PromptSurface::MainChat, &paths, &cfg, "muse-spark-1.3");
        input.allow_host_desktop = true;
        input.workspace_vm_enabled = false;
        input.skills_inventory = Some("- `goals` — Manage durable goals and milestones.");
        input.goals_summary = Some("- `launch-v04`: Ship Grok-Muse v0.4.0 (active)");
        input.now = DateTime::parse_from_rfc3339("2026-11-15T01:25:06Z")
            .unwrap()
            .with_timezone(&Utc);

        let rendered = MusePromptBuilder::build(&input).unwrap();
        let combined = format!(
            "{}\n\n---\n{}\n",
            rendered.messages[0].content, rendered.messages[1].content
        );

        let snapshot_path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots/main_chat.txt");
        if std::env::var("UPDATE_SNAPSHOTS").as_deref() == Ok("1") || !snapshot_path.exists() {
            std::fs::create_dir_all(snapshot_path.parent().unwrap()).unwrap();
            std::fs::write(&snapshot_path, &combined).unwrap();
        }
        let expected = std::fs::read_to_string(&snapshot_path).unwrap();
        assert_eq!(
            combined.replace("\r\n", "\n"),
            expected.replace("\r\n", "\n"),
            "MainChat snapshot drifted; inspect diff or regenerate intentionally"
        );
    }

    #[test]
    fn auto_skills_inventory_populates_when_enabled_and_omitted_when_disabled() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = tmp.path().join("storage");
        let paths = MuseRuntimePaths::shared(&storage);
        bootstrap_shared_runtime_files(&paths).unwrap();

        let enabled_cfg = MuseConfig::default();
        let input_on = PromptInput::new(PromptSurface::MainChat, &paths, &enabled_cfg, "grok-4");
        let rendered_on = MusePromptBuilder::build(&input_on).unwrap();
        assert!(
            rendered_on.messages[0]
                .content
                .contains("## Available Skills & Connectors")
        );
        assert!(rendered_on.messages[0].content.contains("`goals`"));
        assert!(rendered_on.messages[0].content.contains("`forget`"));

        let disabled_cfg = MuseConfig {
            enable_skills: false,
            ..Default::default()
        };
        let input_off = PromptInput::new(PromptSurface::MainChat, &paths, &disabled_cfg, "grok-4");
        let rendered_off = MusePromptBuilder::build(&input_off).unwrap();
        assert!(
            !rendered_off.messages[0]
                .content
                .contains("## Available Skills & Connectors")
        );
    }
}
