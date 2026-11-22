//! GrokBot Global Keyboard Shortcuts & Chord Dispatcher.
//!
//! Supported chords (`Cmd` on macOS / `Ctrl` on Linux & Windows):
//! - `Cmd/Ctrl + K`: Command Palette
//! - `Cmd/Ctrl + ,`: Settings Modal
//! - `Cmd/Ctrl + B`: Toggle Sidebar Collapse
//! - `Cmd/Ctrl + Shift + F`: Expand Sidebar & Open Conversation Search
//! - `Cmd/Ctrl + Shift + M`: Studio Modal (Templates, Skills, Routines, Sections)
//! - `Cmd/Ctrl + /`: Keyboard Shortcuts Cheat-Sheet
//! - `Cmd/Ctrl + Alt + B`: Toggle 340px `Bot Details` Drawer
//! - `Cmd/Ctrl + F`: Toggle in-chat two-stage FTS5 search bar
//! - `Cmd/Ctrl + D`: Start/Stop Dictation session
//! - `Cmd/Ctrl + 1..5`: Switch Workspace Section (Bots, Feed, Goals, Pages, Audit)
//! - `Cmd/Ctrl + 6..9`: Switch to Bot 6..9 in roster order
//! - `Escape`: 5-tier Escape dismissal priority stack

use crate::ui::state::{ActiveModal, GrokUiState, WorkspaceSection};
use eframe::egui::{self, Key, Modifiers};

pub struct HotkeyOutcome {
    pub switch_to_bot_index: Option<usize>,
    pub escape_dismissed: bool,
    pub sidebar_collapsed_toggled: Option<bool>,
    pub toggle_dictation: bool,
}

pub fn handle_global_hotkeys(ctx: &egui::Context, ui_state: &mut GrokUiState) -> HotkeyOutcome {
    let mut outcome = HotkeyOutcome {
        switch_to_bot_index: None,
        escape_dismissed: false,
        sidebar_collapsed_toggled: None,
        toggle_dictation: false,
    };

    // 5-tier Escape dismissal stack
    if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
        outcome.escape_dismissed = ui_state.dismiss_escape_layer();
        return outcome;
    }

    if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::N)) {
        ui_state.active_modal = Some(ActiveModal::CreateBot);
    }
    // Cmd/Ctrl + B -> Toggle sidebar collapsed state (`Some(false)` == collapsed)
    if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::B)) {
        let was_collapsed = ui_state.sidebar_manual_override == Some(false);
        let now_collapsed = !was_collapsed;
        ui_state.sidebar_manual_override = Some(!now_collapsed);
        outcome.sidebar_collapsed_toggled = Some(now_collapsed);
    }
    // Cmd/Ctrl + Shift + F -> Expand sidebar and open conversation search
    if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::F)) {
        ui_state.sidebar_manual_override = Some(true);
        ui_state.sidebar_search_open = true;
    }
    if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::OpenBracket)) {
        ui_state.nav_request = -1;
    }
    if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::CloseBracket)) {
        ui_state.nav_request = 1;
    }

    if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::A)) {
        ui_state.active_modal = Some(ActiveModal::AttachFile);
    }
    // Cmd/Ctrl + K -> Command Palette
    if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::K)) {
        ui_state.active_modal = if ui_state.active_modal == Some(ActiveModal::CommandPalette) {
            None
        } else {
            ui_state.command_palette_query.clear();
            ui_state.command_palette_selected = 0;
            Some(ActiveModal::CommandPalette)
        };
        return outcome;
    }

    // Cmd/Ctrl + , -> Settings Modal
    if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Comma)) {
        ui_state.active_modal = if ui_state.active_modal == Some(ActiveModal::Settings) {
            None
        } else {
            Some(ActiveModal::Settings)
        };
        return outcome;
    }

    // Cmd/Ctrl + Shift + M -> Studio Modal (Templates, Skills, Routines, Sections)
    if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::M)) {
        ui_state.active_modal = if ui_state.active_modal == Some(ActiveModal::Studio) {
            None
        } else {
            Some(ActiveModal::Studio)
        };
        return outcome;
    }

    // Cmd/Ctrl + / -> Keyboard Shortcuts Modal
    if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Slash)) {
        ui_state.active_modal = if ui_state.active_modal == Some(ActiveModal::Shortcuts) {
            None
        } else {
            Some(ActiveModal::Shortcuts)
        };
        return outcome;
    }

    // Cmd/Ctrl + Alt + B -> Toggle Bot Details Drawer
    if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND.plus(Modifiers::ALT), Key::B)) {
        ui_state.details_drawer_open = !ui_state.details_drawer_open;
        return outcome;
    }

    // Cmd/Ctrl + F -> Toggle in-chat Search bar
    if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::F)) {
        ui_state.search_bar_open = !ui_state.search_bar_open;
        return outcome;
    }

    // Cmd/Ctrl + D -> Toggle real Dictation session via daemon voice controller
    if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::D)) {
        outcome.toggle_dictation = true;
        return outcome;
    }

    // Cmd/Ctrl + 1..5 -> Workspace Sections; Cmd/Ctrl + 6..9 -> Switch to Bot N
    let digit_keys = [
        Key::Num1,
        Key::Num2,
        Key::Num3,
        Key::Num4,
        Key::Num5,
        Key::Num6,
        Key::Num7,
        Key::Num8,
        Key::Num9,
    ];
    for (idx, key) in digit_keys.into_iter().enumerate() {
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, key)) {
            if idx < 5 {
                ui_state.section = WorkspaceSection::ALL[idx];
            } else {
                ui_state.section = WorkspaceSection::Bots;
                ui_state.active_chat_kind = "dm".into();
                outcome.switch_to_bot_index = Some(idx);
            }
            break;
        }
    }

    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_hotkey(modifiers: Modifiers, key: Key, state: &mut GrokUiState) -> HotkeyOutcome {
        let ctx = egui::Context::default();
        let mut raw = egui::RawInput {
            modifiers,
            ..Default::default()
        };
        raw.events.push(egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
            modifiers,
        });
        let mut outcome = HotkeyOutcome {
            switch_to_bot_index: None,
            escape_dismissed: false,
            sidebar_collapsed_toggled: None,
            toggle_dictation: false,
        };
        let _ = ctx.run(raw, |ctx| {
            outcome = handle_global_hotkeys(ctx, state);
        });
        outcome
    }

    #[test]
    fn hotkeys_open_studio_toggle_sidebar_open_search_and_trigger_dictation() {
        let mut st = GrokUiState::default();

        // Cmd+Shift+M opens Studio (not ConnectApps).
        run_hotkey(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::M, &mut st);
        assert_eq!(st.active_modal, Some(ActiveModal::Studio));

        // Cmd+B collapses expanded sidebar on first press and expands on second press.
        let out1 = run_hotkey(Modifiers::COMMAND, Key::B, &mut st);
        assert_eq!(st.sidebar_manual_override, Some(false));
        assert_eq!(out1.sidebar_collapsed_toggled, Some(true));

        let out2 = run_hotkey(Modifiers::COMMAND, Key::B, &mut st);
        assert_eq!(st.sidebar_manual_override, Some(true));
        assert_eq!(out2.sidebar_collapsed_toggled, Some(false));

        // Cmd+Shift+F expands sidebar AND opens sidebar conversation search.
        st.sidebar_manual_override = Some(false);
        st.sidebar_search_open = false;
        run_hotkey(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::F, &mut st);
        assert_eq!(st.sidebar_manual_override, Some(true));
        assert!(st.sidebar_search_open);

        // Cmd+D requests real dictation toggle instead of silently flipping a dead bool.
        let out_d = run_hotkey(Modifiers::COMMAND, Key::D, &mut st);
        assert!(out_d.toggle_dictation);
    }
}
