//! Workspace routing shared by the sidebar and command palette.
use crate::ui::state::{GrokUiState, WorkspaceSection};
use gm_core::ipc::DaemonRequest;

pub fn select_section(
    ui_state: &mut GrokUiState,
    sec: WorkspaceSection,
    send: &mut impl FnMut(DaemonRequest),
) {
    ui_state.section = sec;
    match sec {
        WorkspaceSection::Bots => {}
        WorkspaceSection::Feed => send(DaemonRequest::ListActivity),
        WorkspaceSection::Goals => send(DaemonRequest::ListGoals),
        WorkspaceSection::Spaces => send(DaemonRequest::ListSpaces),
        WorkspaceSection::Audit => {
            send(DaemonRequest::ListQueue { bot_id: None });
            send(DaemonRequest::QueryAudit {
                category: None,
                bot_id: None,
                limit: Some(100),
            });
        }
    }
}
