//! GrokBot UI State Machine, Adaptive Sidebar Mode Calculator, Smart Composer (`->` -> `→` + Undo + IME Guard),
//! and 5-Tier `Escape` Dismissal Priority Stack.

use serde_json::Value;
use std::collections::HashMap;
use zeroize::Zeroizing;

/// Top-level workspace sections in the 48px left Workspace Rail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WorkspaceSection {
    #[default]
    Bots,
    Feed,
    Goals,
    Spaces,
    Audit,
}

impl WorkspaceSection {
    pub const ALL: [WorkspaceSection; 5] = [
        WorkspaceSection::Bots,
        WorkspaceSection::Feed,
        WorkspaceSection::Goals,
        WorkspaceSection::Spaces,
        WorkspaceSection::Audit,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            Self::Bots => "Bots",
            Self::Feed => "Feed",
            Self::Goals => "Goals",
            Self::Spaces => "Pages",
            Self::Audit => "Audit",
        }
    }
}

/// In-memory navigation; visiting after Back discards the forward branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavTarget {
    pub section: WorkspaceSection,
    pub chat_kind: String,
    pub target_id: String,
}

#[derive(Default)]
pub struct NavHistory {
    entries: Vec<NavTarget>,
    cursor: usize,
}

impl NavHistory {
    pub fn push(&mut self, target: NavTarget) {
        if self.entries.get(self.cursor) == Some(&target) {
            return;
        }
        self.entries.truncate(if self.entries.is_empty() {
            0
        } else {
            self.cursor + 1
        });
        self.entries.push(target);
        if self.entries.len() > 100 {
            self.entries.remove(0);
        }
        self.cursor = self.entries.len() - 1;
    }
    pub fn can_back(&self) -> bool {
        !self.entries.is_empty() && self.cursor > 0
    }
    pub fn can_forward(&self) -> bool {
        self.cursor + 1 < self.entries.len()
    }
    pub fn back(&mut self) -> Option<NavTarget> {
        if !self.can_back() {
            return None;
        }
        self.cursor -= 1;
        self.entries.get(self.cursor).cloned()
    }
    pub fn forward(&mut self) -> Option<NavTarget> {
        if !self.can_forward() {
            return None;
        }
        self.cursor += 1;
        self.entries.get(self.cursor).cloned()
    }
}

/// Four tabs in the 340px right-hand `Bot Details` Drawer (`Cmd/Ctrl+Alt+B`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BotDetailsTab {
    #[default]
    Settings,
    Computer,
    Routines,
    Memory,
    Library,
}

impl BotDetailsTab {
    pub const ALL: [BotDetailsTab; 4] = [
        BotDetailsTab::Settings,
        BotDetailsTab::Routines,
        BotDetailsTab::Memory,
        BotDetailsTab::Library,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            Self::Settings => "Overview",
            Self::Computer => "Computer",
            Self::Routines => "Routines",
            Self::Memory => "Memory",
            Self::Library => "Library",
        }
    }
}

/// Five panes in the Global Settings Modal (`Cmd/Ctrl+,`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SettingsPane {
    #[default]
    General,
    RulesAndAccess,
    VaultAndPayments,
    UsageAndBudget,
    SystemAndUpdates,
    Providers,
}

impl SettingsPane {
    pub const ALL: [SettingsPane; 6] = [
        SettingsPane::General,
        SettingsPane::Providers,
        SettingsPane::RulesAndAccess,
        SettingsPane::VaultAndPayments,
        SettingsPane::UsageAndBudget,
        SettingsPane::SystemAndUpdates,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Providers => "Providers & models",
            Self::RulesAndAccess => "Rules & Access",
            Self::VaultAndPayments => "Vault & Payments",
            Self::UsageAndBudget => "Usage & Budget",
            Self::SystemAndUpdates => "System & Updates",
        }
    }
}

/// Global modal overlays (Tier 2 of the Escape dismissal stack).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActiveModal {
    CommandPalette,
    Shortcuts,
    Settings,
    Studio,
    ConnectApps,
    CreateBot,
    CreateChannel,
    EditChannel,
    RoutineEditor,
    AttachFile,
    AvatarPicker {
        bot_id: String,
    },
    Lightbox {
        title: String,
        uri: String,
        mime: String,
    },
    ExpandedTable {
        title: String,
        headers: Vec<String>,
        rows: Vec<Vec<String>>,
        raw_markdown: String,
    },
}

/// Target open in the right-hand Split Page / Space Canvas (Tier 3 of the Escape dismissal stack).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SplitPageTarget {
    MarkdownFile {
        bot_id: String,
        file: String,
        title: String,
        editing: bool,
    },
    StaticSpace {
        slug: String,
        title: String,
        url: Option<String>,
        editing_html: bool,
    },
}

/// Reply-to banner state above the Smart Composer (Tier 5 of the Escape dismissal stack).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyBanner {
    pub message_id: String,
    pub author: String,
    pub snippet: String,
}

type SecretCardFields = (Vec<(String, Zeroizing<String>)>, bool);

/// Transient interactive state for all 14 inline cards (keyed by `request_id` or `message_id`).
#[derive(Default)]
pub struct InlineCardUiState {
    /// Approval card: whether `"Always allow"` / custom rule input is checked, plus pattern draft.
    pub approval_remember: HashMap<String, (bool, String)>,
    /// Secret prompt card: zeroized field values `request_id -> Vec<(field_key, Zeroizing<String>)>` + `save_to_vault`.
    pub secret_fields: HashMap<String, SecretCardFields>,
    /// Clarifying question card: selected option labels + `"Other"` freeform text.
    pub question_answers: HashMap<String, (Vec<String>, String)>,
    /// Expanded file diff / output cards (`request_id -> expanded`).
    /// Take-the-wheel handoff resume notes (`task_id -> note`).
    pub wheel_notes: HashMap<String, String>,
}

impl InlineCardUiState {
    /// Zeroize and drop secret prompt inputs for a given `request_id` (`RULE-SECRET-001`).
    pub fn clear_secret(&mut self, request_id: &str) {
        if let Some((mut fields, _)) = self.secret_fields.remove(request_id) {
            for (_, val) in &mut fields {
                use zeroize::Zeroize;
                val.zeroize();
            }
        }
    }
}

/// Complete GrokBot UI state held on `NativeApp`.
pub struct GrokUiState {
    pub section: WorkspaceSection,
    pub server_snapshot: Value,
    pub responses: Value,
    pub explain_query: String,
    pub discretion_chat: String,
    pub discretion_text: String,
    pub discretion_sha: Option<String>,
    pub discretion_loosens: bool,
    pub forget_confirmed: bool,
    pub forget_verify: String,
    pub publish_confirmed: bool,
    pub nav_history: NavHistory,
    pub nav_request: i8,
    pub sidebar_manual_override: Option<bool>,
    pub show_hidden_bots: bool,
    pub sidebar_search: String,
    pub sidebar_search_open: bool,
    pub sidebar_width: f32,
    pub sidebar_saved_width: f32,
    pub details_saved_width: f32,
    pub last_read_seq: HashMap<String, i64>,
    pub details_width: f32,
    pub transcript_heights: eframe::egui::ahash::HashMap<String, f32>,
    pub transcript_width: f32,
    pub provider_opened_attempt: String,
    pub provider_poll_at: std::time::Instant,

    pub active_chat_kind: String, // "dm" or "channel"
    pub details_drawer_open: bool,
    pub details_tab: BotDetailsTab,
    pub live_computer_open: bool,
    pub live_computer_fullscreen: bool,
    pub split_page: Option<SplitPageTarget>,
    pub active_modal: Option<ActiveModal>,
    pub settings_pane: SettingsPane,

    // Tier 1 transient popovers
    pub emoji_picker_msg: Option<String>,
    pub message_actions_id: Option<String>,
    pub mention_popup_open: bool,
    pub slash_popup_open: bool,
    pub context_menu_bot: Option<String>,
    pub table_copy_menu: Option<String>,

    // Tier 5 composer / search overlays
    pub reply_to: Option<ReplyBanner>,
    pub search_bar_open: bool,
    pub search_older_tier2: bool,

    // Smart composer state
    pub last_auto_arrow_pos: Option<usize>,
    pub ime_composing: bool,
    pub voice_active: bool,
    pub voice_recording: bool,
    pub dictation_active: bool,
    pub pending_attachments: Vec<String>,

    // Command palette & modal drafts
    pub command_palette_query: String,
    pub command_palette_selected: usize,
    pub studio_tab: String, // "templates", "skills", "routines", "sections"
    pub template_export_name: String,
    pub template_export_desc: String,
    pub template_import_json: String,
    pub custom_skill_name: String,
    pub custom_skill_title: String,
    pub custom_skill_desc: String,
    pub custom_skill_body: String,
    pub new_section_name: String,
    pub routine_draft: Value,
    pub channel_draft: Value,
    pub feed_post_title: String,
    pub feed_post_body: String,
    pub feed_prompt: String,
    pub idea_prompt: String,
    pub feedback_note: String,
    pub diagnostics_consent: bool,
    pub bot_profile_draft: Option<(String, Value)>,

    // Settings modal drafts
    pub theme_pref: String,
    pub accent_mode: String,
    pub execution_mode: String,
    pub sleep_prevention: bool,
    pub wake_at_login: bool,
    pub hardware_security_keys: bool,
    pub rule_mode_draft: String,
    pub rule_pattern_draft: String,
    pub rule_desc_draft: String,
    pub vault_kind_draft: String,
    pub vault_label_draft: String,
    pub vault_username_draft: String,
    pub vault_domain_draft: String,
    pub vault_secret_draft: Zeroizing<String>,
    pub vault_csv_draft: String,
    pub revealed_secrets: HashMap<String, String>,

    // Connect Apps / Custom connector drafts
    pub connector_name_draft: String,
    pub connector_kind_draft: String,
    pub connector_endpoint_draft: String,
    pub connector_args_draft: String,
    pub connector_header_draft: String,
    pub connector_account_draft: String,
    pub connector_secret_draft: Zeroizing<String>,

    // Teach a Task state
    pub teach_active: bool,
    pub teach_step_draft: String,
    pub teach_skill_name_draft: String,
    pub teach_steps_recorded: Vec<String>,

    // Library pagination state
    pub library_filter: String,
    pub library_cursor: usize,

    // Create Bot / Avatar Picker drafts
    pub new_bot_id: String,
    pub new_bot_name: String,
    pub new_bot_role: String,
    pub new_bot_prompt: String,
    pub avatar_shape_draft: String,
    pub avatar_color_draft: String,
    pub avatar_upload_path: String,
    pub attachment_path: String,

    // Applied style fingerprint so `ctx.set_style` only runs on actual changes
    pub applied_style_key: String,
    pub media_zoom: f32,

    pub provider_selected_id: String,
    pub provider_api_key_draft: Zeroizing<String>,
    pub provider_oauth_token_draft: Zeroizing<String>,
    pub provider_revalidate_check: bool,

    pub cards: InlineCardUiState,
}

impl Default for GrokUiState {
    fn default() -> Self {
        Self {
            section: WorkspaceSection::Bots,
            server_snapshot: Value::Null,
            responses: serde_json::json!({}),
            explain_query: String::new(),
            discretion_chat: String::new(),
            discretion_text: String::new(),
            discretion_sha: None,
            discretion_loosens: false,
            forget_confirmed: false,
            forget_verify: String::new(),
            publish_confirmed: false,
            nav_history: NavHistory::default(),
            nav_request: 0,
            sidebar_manual_override: None,
            show_hidden_bots: false,
            sidebar_search: String::new(),
            sidebar_search_open: false,
            sidebar_width: 316.0,
            sidebar_saved_width: 316.0,
            details_saved_width: 384.0,
            last_read_seq: HashMap::new(),
            details_width: 384.0,
            transcript_heights: Default::default(),
            transcript_width: 0.0,
            provider_opened_attempt: String::new(),
            provider_poll_at: std::time::Instant::now(),
            active_chat_kind: "dm".into(),
            details_drawer_open: false,
            details_tab: BotDetailsTab::Settings,
            live_computer_open: false,
            live_computer_fullscreen: false,
            split_page: None,
            active_modal: None,
            settings_pane: SettingsPane::General,
            emoji_picker_msg: None,
            message_actions_id: None,
            mention_popup_open: false,
            slash_popup_open: false,
            context_menu_bot: None,
            table_copy_menu: None,
            reply_to: None,
            search_bar_open: false,
            search_older_tier2: false,
            last_auto_arrow_pos: None,
            ime_composing: false,
            voice_active: false,
            voice_recording: false,
            dictation_active: false,
            pending_attachments: Vec::new(),
            command_palette_query: String::new(),
            command_palette_selected: 0,
            studio_tab: "templates".into(),
            template_export_name: "My GrokBot Team".into(),
            template_export_desc: "Portable multi-bot team template".into(),
            template_import_json: String::new(),
            custom_skill_name: String::new(),
            custom_skill_title: String::new(),
            custom_skill_desc: String::new(),
            custom_skill_body: String::new(),
            new_section_name: String::new(),
            routine_draft: Value::Null,
            channel_draft: Value::Null,
            feed_post_title: String::new(),
            feed_post_body: String::new(),
            feed_prompt: String::new(),
            idea_prompt: String::new(),
            feedback_note: String::new(),
            diagnostics_consent: false,
            bot_profile_draft: None,
            theme_pref: "system".into(),
            accent_mode: "bot_color".into(),
            execution_mode: "ask".into(),
            sleep_prevention: false,
            wake_at_login: false,
            hardware_security_keys: false,
            rule_mode_draft: "allow".into(),
            rule_pattern_draft: String::new(),
            rule_desc_draft: String::new(),
            vault_kind_draft: "login".into(),
            vault_label_draft: String::new(),
            vault_username_draft: String::new(),
            vault_domain_draft: String::new(),
            vault_secret_draft: Zeroizing::new(String::new()),
            vault_csv_draft: String::new(),
            revealed_secrets: HashMap::new(),
            connector_name_draft: String::new(),
            connector_kind_draft: "remote_https".into(),
            connector_endpoint_draft: String::new(),
            connector_args_draft: "[]".into(),
            connector_header_draft: "Authorization".into(),
            connector_account_draft: String::new(),
            connector_secret_draft: Zeroizing::new(String::new()),
            teach_active: false,
            teach_step_draft: String::new(),
            teach_skill_name_draft: String::new(),
            teach_steps_recorded: Vec::new(),
            library_filter: "all".into(),
            library_cursor: 0,
            new_bot_id: String::new(),
            new_bot_name: String::new(),
            new_bot_role: "Specialist".into(),
            new_bot_prompt: "You are a helpful specialist agent.".into(),
            avatar_shape_draft: "Circle".into(),
            avatar_color_draft: "#2F7CF6".into(),
            avatar_upload_path: String::new(),
            attachment_path: String::new(),
            applied_style_key: String::new(),
            media_zoom: 1.0,
            provider_selected_id: "chatgpt-default".into(),
            provider_api_key_draft: Zeroizing::new(String::new()),
            provider_oauth_token_draft: Zeroizing::new(String::new()),
            provider_revalidate_check: false,
            cards: InlineCardUiState::default(),
        }
    }
}

impl GrokUiState {
    /// Dismiss the highest-priority active UI layer according to the 5-tier GrokBot Escape stack:
    /// Tier 1: Active transient popover (emoji picker, `@`/`/` autocomplete, context menu, table copy menu)
    /// Tier 2: Active modal (`CommandPalette`, `Shortcuts`, `Settings`, `Studio`, `AvatarPicker`, `Lightbox`, `ExpandedTable`, etc.)
    /// Tier 3: Active `SplitPage` or `LiveComputer` viewer
    /// Tier 4: Open `BotDetails` drawer
    /// Tier 5: Active `reply_to` banner or `search_bar_open`
    pub fn dismiss_escape_layer(&mut self) -> bool {
        // Tier 1: Popovers
        if self.emoji_picker_msg.is_some()
            || self.mention_popup_open
            || self.slash_popup_open
            || self.context_menu_bot.is_some()
            || self.table_copy_menu.is_some()
        {
            self.emoji_picker_msg = None;
            self.mention_popup_open = false;
            self.slash_popup_open = false;
            self.context_menu_bot = None;
            self.table_copy_menu = None;
            return true;
        }

        // Tier 2: Modals
        if self.active_modal.is_some() {
            use zeroize::Zeroize;
            self.vault_secret_draft.zeroize();
            self.connector_secret_draft.zeroize();
            self.vault_csv_draft.zeroize();
            self.provider_api_key_draft.zeroize();
            self.provider_oauth_token_draft.zeroize();
            for secret in self.revealed_secrets.values_mut() {
                secret.zeroize();
            }
            self.revealed_secrets.clear();
            self.cards.secret_fields.clear();
            self.active_modal = None;
            return true;
        }

        // Tier 3: Split Page or Live Computer
        if self.live_computer_fullscreen || self.live_computer_open {
            self.live_computer_fullscreen = false;
            self.live_computer_open = false;
            return true;
        }
        if self.split_page.is_some() {
            self.split_page = None;
            return true;
        }

        // Tier 4: Bot Details Drawer
        if self.details_drawer_open {
            self.details_drawer_open = false;
            return true;
        }

        // Tier 5: Reply-to banner or inline Search bar
        if self.reply_to.is_some() {
            self.reply_to = None;
            return true;
        }
        if self.search_bar_open {
            self.search_bar_open = false;
            return true;
        }

        false
    }

    /// Returns true if the Live Computer viewer is currently visible on screen.
    pub fn is_live_computer_visible(&self) -> bool {
        self.live_computer_open
            || self.live_computer_fullscreen
            || (self.details_drawer_open && self.details_tab == BotDetailsTab::Computer)
    }

    /// Occupied width of the left sidebar at the current viewport width.
    pub fn sidebar_occupied_width(&self, viewport_width: f32) -> f32 {
        let collapsed = self.sidebar_manual_override == Some(false);
        let narrow = viewport_width < 760.0;
        if collapsed || narrow {
            72.0
        } else {
            self.sidebar_width
        }
    }

    /// Use a scrollable overlay whenever docking a side drawer would leave under 320 pt for chat.
    pub fn should_overlay_right_panel(
        &self,
        viewport_width: f32,
        panel_width: f32,
        extra_occupied: f32,
    ) -> bool {
        let left = self.sidebar_occupied_width(viewport_width);
        viewport_width - left - extra_occupied - panel_width < 320.0
    }
}

/// Apply GrokBot Smart Composer transformations:
/// - When `!ime_composing`, converts `"->"` into `"→"` and records the byte offset in `last_auto_arrow_pos`.
/// - When `backspace_pressed` immediately after an auto-arrow replacement (where the `"→"` was just deleted
///   or sits at the end), restores the literal `"->"` so a single Backspace undoes the auto-arrow.
pub fn apply_smart_composer_edit(
    draft: &mut String,
    ime_composing: bool,
    backspace_pressed: bool,
    last_auto_arrow_pos: &mut Option<usize>,
) -> bool {
    if ime_composing {
        return false;
    }

    if backspace_pressed && let Some(pos) = *last_auto_arrow_pos {
        // Case A: egui TextEdit already deleted the '→' character at `pos` on Backspace
        if draft.len() == pos && draft.is_char_boundary(pos) {
            draft.push_str("->");
            *last_auto_arrow_pos = None;
            return true;
        }
        // Case B: caller invoked undo before deleting '→' at `pos`
        let arrow_len = '→'.len_utf8();
        if pos + arrow_len <= draft.len() && draft.get(pos..pos + arrow_len) == Some("->") {
            *last_auto_arrow_pos = None;
        } else if pos + arrow_len <= draft.len() && draft.get(pos..pos + arrow_len) == Some("→") {
            draft.replace_range(pos..pos + arrow_len, "->");
            *last_auto_arrow_pos = None;
            return true;
        }
    }

    // Backtick runs delimit inline and fenced code; only equal runs close a span.
    let bytes = draft.as_bytes();
    let mut code_ticks = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            let start = i;
            while i < bytes.len() && bytes[i] == b'`' {
                i += 1;
            }
            let count = i - start;
            if code_ticks == 0 {
                code_ticks = count;
            } else if code_ticks == count {
                code_ticks = 0;
            }
            continue;
        }
        if code_ticks == 0 && bytes[i..].starts_with(b"->") {
            draft.replace_range(i..i + 2, "→");
            *last_auto_arrow_pos = Some(i);
            return true;
        }
        i += 1;
    }
    if !backspace_pressed
        && let Some(pos) = *last_auto_arrow_pos
        && !draft.get(pos..).is_some_and(|s| s.starts_with('→'))
    {
        *last_auto_arrow_pos = None;
    }

    false
}

#[cfg(test)]
mod regression_tests {
    use super::*;

    #[test]
    fn arrows_preserve_inline_fenced_and_unicode_code() {
        for code in [
            "`->`",
            "```rust\nfn f() -> u8 {}\n```",
            "`` ` -> ``",
            "`é->`",
            "```\n->",
        ] {
            let mut draft = format!("{code} then ->");
            let mut position = None;
            let changed = apply_smart_composer_edit(&mut draft, false, false, &mut position);
            if code == "```\n->" {
                assert!(!changed);
            } else {
                assert!(changed);
                assert_eq!(draft, format!("{code} then →"));
                assert!(apply_smart_composer_edit(
                    &mut draft,
                    false,
                    true,
                    &mut position
                ));
                assert_eq!(draft, format!("{code} then ->"));
            }
        }
        let mut draft = "é".to_string();
        assert!(!apply_smart_composer_edit(
            &mut draft,
            false,
            false,
            &mut Some(1)
        ));
    }

    #[test]
    fn escape_zeroizes_modal_secrets_but_preserves_layer_priority() {
        let mut state = GrokUiState {
            active_modal: Some(ActiveModal::Settings),
            mention_popup_open: true,
            vault_secret_draft: Zeroizing::new("vault secret".into()),
            connector_secret_draft: Zeroizing::new("connector secret".into()),
            vault_csv_draft: "password export".into(),
            ..Default::default()
        };
        state
            .revealed_secrets
            .insert("id".into(), "revealed".into());
        assert!(state.dismiss_escape_layer());
        assert!(state.active_modal.is_some());
        assert!(state.dismiss_escape_layer());
        assert!(state.vault_secret_draft.is_empty());
        assert!(state.connector_secret_draft.is_empty());
        assert!(state.vault_csv_draft.is_empty());
        assert!(state.revealed_secrets.is_empty());
    }
}

#[cfg(test)]
mod navigation_tests {
    use super::*;
    #[test]
    fn navigation_branches_and_duplicate_visits() {
        let target = |id: &str| NavTarget {
            section: WorkspaceSection::Bots,
            chat_kind: "dm".into(),
            target_id: id.into(),
        };
        let mut history = NavHistory::default();
        history.push(target("muse"));
        history.push(target("muse"));
        assert!(!history.can_back());
        history.push(target("coder"));
        assert_eq!(history.back(), Some(target("muse")));
        assert!(history.can_forward());
        assert_eq!(history.forward(), Some(target("coder")));
        history.back();
        history.push(target("researcher"));
        assert!(!history.can_forward());
        assert_eq!(history.back(), Some(target("muse")));
    }

    #[test]
    fn drawer_uses_scrollable_overlay_below_320_pt_chat_floor() {
        let state = GrokUiState::default();
        assert!(state.should_overlay_right_panel(680.0, 384.0, 0.0));
        assert!(!state.should_overlay_right_panel(1220.0, 384.0, 0.0));
        assert!(state.should_overlay_right_panel(1220.0, 420.0, 384.0));
    }
}
