//! GrokBot Global Keyboard Shortcuts & Chord Dispatcher.
//!
//! Supported chords (`Cmd` on macOS / `Ctrl` on Linux & Windows):
//! - `Cmd/Ctrl + K`: Command Palette
//! - `Cmd/Ctrl + ,`: Settings Modal (5 panes)
//! - `Cmd/Ctrl + Shift + M`: Studio Modal (Templates, Skills, Routines, Sections)
//! - `Cmd/Ctrl + /`: Keyboard Shortcuts Cheat-Sheet
//! - `Cmd/Ctrl + Alt + B`: Toggle 340px `Bot Details` Drawer
//! - `Cmd/Ctrl + F`: Toggle in-chat two-stage FTS5 search bar
//! - `Cmd/Ctrl + D`: Toggle Dictation (`[Dictate]` badge)
//! - `Cmd/Ctrl + 1..9`: Switch to Bot 1..9 in roster order
//! - `Escape`: 5-tier Escape dismissal priority stack

use crate::ui::state::{ActiveModal, GrokUiState, WorkspaceSection};
use eframe::egui::{self, Key, Modifiers};

pub struct HotkeyOutcome {
    pub switch_to_bot_index: Option<usize>,
    pub escape_dismissed: bool,
}

pub fn handle_global_hotkeys(ctx: &egui::Context, ui_state: &mut GrokUiState) -> HotkeyOutcome {
    let mut outcome = HotkeyOutcome {
        switch_to_bot_index: None,
        escape_dismissed: false,
    };

    // 5-tier Escape dismissal stack
    if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
        outcome.escape_dismissed = ui_state.dismiss_escape_layer();
        return outcome;
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

    // Cmd/Ctrl + Shift + M -> Studio Modal
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

    // Cmd/Ctrl + D -> Toggle Dictation
    if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::D)) {
        ui_state.dictation_active = !ui_state.dictation_active;
        return outcome;
    }

    // Cmd/Ctrl + 1..9 -> Switch to Bot N
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
            ui_state.section = WorkspaceSection::Bots;
            ui_state.active_chat_kind = "dm".into();
            outcome.switch_to_bot_index = Some(idx);
            break;
        }
    }

    outcome
}
