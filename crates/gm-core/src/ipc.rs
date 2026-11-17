use crate::error::{GmError, Result};
use crate::types::{
    AxSnapshot, CalendarItem, ComputerAction, FrameCapture, MailItem, PermissionStatus,
    ReminderItem, UserActivity,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcMessage<T> {
    pub id: u64,
    pub payload: T,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum HelperRequest {
    Ping,
    GetPermissions,
    GetUserActivity,
    CaptureLatestFrame {
        max_long_edge: Option<u32>,
        quality: Option<u8>,
    },
    GetAxSnapshot {
        window_id: Option<u32>,
    },
    CaptureTaskFrame {
        task_id: String,
        max_long_edge: Option<u32>,
        quality: Option<u8>,
    },
    GetTaskAxSnapshot {
        task_id: String,
        window_id: Option<u32>,
    },
    BeginTask {
        task_id: String,
    },
    ExecuteTaskAction {
        task_id: String,
        action: ComputerAction,
    },
    CancelTask {
        task_id: String,
    },
    ExecuteAction(ComputerAction),
    CancelActions,
    GetCalendarEvents {
        start_time_ms: u64,
        end_time_ms: u64,
    },
    GetReminders {
        include_completed: bool,
    },
    GetUnreadEmails,
    SendNotification {
        title: String,
        subtitle: Option<String>,
        body: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", content = "result")]
pub enum HelperResponse {
    Pong,
    Permissions(PermissionStatus),
    UserActivity(UserActivity),
    Frame(FrameCapture),
    AxSnapshot(AxSnapshot),
    ActionExecuted {
        success: bool,
        message: Option<String>,
    },
    Cancelled,
    CalendarEvents(Vec<CalendarItem>),
    Reminders(Vec<ReminderItem>),
    UnreadEmails(Vec<MailItem>),
    NotificationSent(bool),
    Error(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "action", content = "params")]
pub enum QueueControlAction {
    Pause,
    Resume,
    Cancel,
    Reassign { target_bot_id: String },
    Retry,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatTurn {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum DaemonRequest {
    Ping,
    GetStatus,
    SubmitTask {
        instruction: String,
    },
    SendChatMessage {
        text: String,
    },
    SendChatMessageV1 {
        message_id: String,
        text: String,
    },
    SendChatMessageV2 {
        message_id: String,
        text: String,
        history: Vec<ChatTurn>,
    },
    SetPaused {
        paused: bool,
    },
    EmergencyKill,
    RespondApproval {
        request_id: String,
        approved: bool,
    },
    SubscribeEvents,
    ImportAttachment {
        bot_id: String,
        source_path: PathBuf,
    },
    ListBotFiles {
        bot_id: String,
    },
    PreviewBotFile {
        bot_id: String,
        path: String,
    },
    ListRoutines,
    SaveRoutine {
        routine: serde_json::Value,
    },
    ListBots,
    SaveBot {
        profile: serde_json::Value,
    },
    ListChannels,
    ChannelHistory {
        channel_id: String,
    },
    CreateChannel {
        channel_id: String,
        topic: String,
        owner_bot_id: Option<String>,
        participants: Vec<String>,
    },
    QueueTask {
        bot_id: Option<String>,
        channel_id: Option<String>,
        instruction: String,
        priority: i32,
    },
    QueueControl {
        task_id: String,
        action: QueueControlAction,
    },
    ListQueue {
        bot_id: Option<String>,
    },
    ChannelPost {
        channel_id: String,
        author: String,
        content: String,
    },
    TakeTheWheelResume {
        task_id: String,
        note: Option<String>,
    },
    ListActivity,
    RateActivity {
        id: String,
        rating: i32,
    },
    ListGoals,
    SaveGoal {
        op: String,
        #[serde(default)]
        slug: Option<String>,
        #[serde(default)]
        payload: serde_json::Value,
    },
    ListSpaces,
    CreateSpace {
        slug: String,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        kind: Option<String>,
        #[serde(default)]
        html: Option<String>,
    },
    SearchSkills {
        #[serde(default)]
        query: String,
    },
    ReadRuntimeFile {
        #[serde(default)]
        bot_id: Option<String>,
        file: String,
    },
    SaveRuntimeFile {
        #[serde(default)]
        bot_id: Option<String>,
        file: String,
        content: String,
        expected_sha256: Option<String>,
    },
    ImportMemory {
        input: String,
        #[serde(default)]
        apply: bool,
        #[serde(default)]
        report_sha256: Option<String>,
    },
    ForgetPlan {
        #[serde(default)]
        bot_id: Option<String>,
        request: String,
    },
    ForgetConfirm {
        #[serde(default)]
        bot_id: Option<String>,
        plan_id: String,
        #[serde(default)]
        verification_query: Option<String>,
        #[serde(default)]
        confirmed: bool,
    },
    UpdateDiscretion {
        chat_id: String,
        rules: Vec<String>,
        #[serde(default)]
        expected_sha256: Option<String>,
    },
    GetBootstrapState,
    GetUiSettings,
    SaveUiSettings {
        ui: serde_json::Value,
    },
    SetBotUiPrefs {
        bot_id: String,
        prefs: serde_json::Value,
    },
    ManageBotSection {
        op: String,
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        collapsed: Option<bool>,
        #[serde(default)]
        sort_order: Option<i32>,
    },
    GetDirectChatHistory {
        bot_id: String,
        #[serde(default)]
        before_seq: Option<u64>,
        #[serde(default)]
        limit: Option<usize>,
    },
    PostDirectChat {
        bot_id: String,
        content: String,
        #[serde(default)]
        reply_to_id: Option<String>,
        #[serde(default)]
        thread_id: Option<String>,
    },
    SearchMessages {
        query: String,
        #[serde(default)]
        include_older: bool,
        #[serde(default)]
        bot_id: Option<String>,
    },
    MarkChatRead {
        chat_kind: String,
        chat_id: String,
    },
    ToggleMessageReaction {
        chat_kind: String,
        message_id: String,
        emoji: String,
        #[serde(default)]
        actor: Option<String>,
    },
    SubmitSecretPrompt {
        request_id: String,
        bot_id: String,
        fields: Vec<(String, String)>,
        #[serde(default)]
        save_to_vault: bool,
        #[serde(default)]
        vault_label: Option<String>,
        #[serde(default)]
        domain: Option<String>,
    },
    RespondQuestion {
        request_id: String,
        answers: Vec<String>,
        #[serde(default)]
        custom_text: Option<String>,
    },
    ManageCustomRule {
        op: String,
        #[serde(default)]
        rule: Option<serde_json::Value>,
        #[serde(default)]
        id: Option<String>,
    },
    ManageVault {
        op: String,
        #[serde(default)]
        item: Option<serde_json::Value>,
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        reveal_confirmed: bool,
    },
    ImportVaultCsv {
        csv: String,
        #[serde(default)]
        source_hint: Option<String>,
    },
    GetUsageBreakdown {
        #[serde(default)]
        days: Option<u32>,
    },
    TriggerRoutineTest {
        routine_id: String,
    },
    DeleteRoutine {
        routine_id: String,
    },
    ListRoutineRuns {
        routine_id: String,
        #[serde(default)]
        limit: Option<usize>,
    },
    ListLibraryPage {
        bot_id: String,
        #[serde(default)]
        filter: String,
        #[serde(default)]
        cursor: Option<usize>,
        #[serde(default)]
        limit: Option<usize>,
    },
    ListCustomConnectors,
    SaveCustomConnector {
        op: String,
        #[serde(default)]
        connector: Option<serde_json::Value>,
        #[serde(default)]
        id: Option<String>,
    },
    CaptureComputerPreview {
        #[serde(default)]
        max_long_edge: Option<u32>,
        #[serde(default)]
        quality: Option<u8>,
    },
    StartTeachTask {
        bot_id: String,
    },
    StopTeachTask {
        #[serde(default)]
        save_skill_name: Option<String>,
    },
    CheckUpdates,
    QueryAudit {
        #[serde(default)]
        category: Option<String>,
        #[serde(default)]
        bot_id: Option<String>,
        #[serde(default)]
        limit: Option<usize>,
    },
    ExplainMemory {
        target: String,
    },
    GetDiscretion {
        chat_id: String,
    },
    ManageSkill {
        op: String,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        payload: serde_json::Value,
    },
    ManageSpace {
        op: String,
        slug: String,
        #[serde(default)]
        payload: serde_json::Value,
    },
    ManageFeed {
        op: String,
        #[serde(default)]
        payload: serde_json::Value,
    },
    ManageSystem {
        op: String,
        #[serde(default)]
        payload: serde_json::Value,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", content = "result")]
pub enum DaemonResponse {
    Pong,
    Status {
        helper_connected: bool,
        paused: bool,
        active_task: Option<String>,
        daily_spend_usd: f64,
    },
    Accepted {
        id: String,
    },
    ChatReply {
        text: String,
    },
    ChatReplyV1 {
        message_id: String,
        text: String,
    },
    JsonPayload(serde_json::Value),
    Killed,
    Ack,
    Event(DaemonEvent),
    Error(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", content = "data")]
pub enum DaemonEvent {
    HudStateChanged {
        state: String,
        detail: String,
        daily_spend_usd: f64,
    },
    ChatToken {
        delta: String,
        done: bool,
    },
    ChatTokenV1 {
        message_id: String,
        delta: String,
        done: bool,
    },
    TaskProgress {
        task_id: String,
        step: u32,
        action_summary: String,
    },
    ApprovalResolved {
        request_id: String,
    },
    ApprovalRequested {
        request_id: String,
        title: String,
        details: String,
    },
    TaskFinished {
        task_id: String,
        ok: bool,
        message: String,
    },
    WheelHandoffRequested {
        task_id: String,
        bot_id: String,
        blocker: String,
        instructions: String,
    },
    WheelHandoffResumed {
        task_id: String,
        bot_id: String,
    },
    QueueUpdated {
        bot_id: String,
        queued: usize,
        running_task_id: Option<String>,
    },
    ChannelMessage {
        channel_id: String,
        author: String,
        bot_id: Option<String>,
        content: String,
        timestamp: String,
    },
    ActivityUpdated {
        reason: String,
    },
    BotUpdated {
        bot_id: String,
    },
    BotStatusPhrase {
        bot_id: String,
        phrase: Option<String>,
        needs_attention: bool,
    },
    DirectMessageAdded {
        bot_id: String,
        message: serde_json::Value,
    },
    MessageCardUpdated {
        bot_id: String,
        message_id: String,
        card: serde_json::Value,
    },
    MessageReactionUpdated {
        chat_kind: String,
        chat_id: String,
        message_id: String,
        reactions: serde_json::Value,
    },
    RoutineUpdated {
        routine_id: Option<String>,
        bot_id: Option<String>,
    },
    RoutineRunRecorded {
        routine_id: String,
        run: serde_json::Value,
    },
    TaskUpdated {
        task: serde_json::Value,
    },
    ApprovalExpired {
        request_id: String,
    },
    SecretPromptRequested {
        request_id: String,
        bot_id: String,
        title: String,
        domain: Option<String>,
        fields: Vec<String>,
    },
    QuestionRequested {
        request_id: String,
        bot_id: String,
        prompt: String,
        options: Vec<String>,
        allow_multiple: bool,
    },
    OrderApprovalRequested {
        request_id: String,
        bot_id: String,
        merchant: String,
        item_title: String,
        amount_usd: f64,
    },
    BudgetUpdated {
        daily_spent_usd: f64,
        daily_cap_usd: f64,
    },
    ComputerFrameReady {
        width: u32,
        height: u32,
        jpeg_base64: String,
        timestamp_ms: u64,
    },
    NotificationAlert {
        id: String,
        bot_id: Option<String>,
        level: String,
        title: String,
        body: String,
        request_id: Option<String>,
    },
}

/// Verify peer UID credential against expected UID (e.g. current effective UID).
pub fn is_peer_authorized(peer_uid: u32, expected_uid: u32) -> bool {
    peer_uid == expected_uid
}

/// Length-prefixed serialization: 4-byte big-endian payload length + JSON bytes.
pub async fn write_ipc_message<W, T>(writer: &mut W, message: &T) -> Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let bytes = serde_json::to_vec(message)?;
    let len = bytes.len() as u32;
    writer.write_u32(len).await?;
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

/// Reads length-prefixed JSON message.
pub async fn read_ipc_message<R, T>(reader: &mut R) -> Result<T>
where
    R: AsyncRead + Unpin,
    T: for<'de> Deserialize<'de>,
{
    let len = reader.read_u32().await?;
    if len > 32 * 1024 * 1024 {
        return Err(GmError::Ipc(format!("Message too large: {len} bytes")));
    }
    let mut buf = vec![0u8; len as usize];
    reader.read_exact(&mut buf).await?;
    let message = serde_json::from_slice(&buf)?;
    Ok(message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[tokio::test]
    async fn test_ipc_roundtrip() {
        let req = IpcMessage {
            id: 42,
            payload: HelperRequest::Ping,
        };
        let mut buffer = Vec::new();
        write_ipc_message(&mut buffer, &req).await.unwrap();

        let mut cursor = Cursor::new(buffer);
        let decoded: IpcMessage<HelperRequest> = read_ipc_message(&mut cursor).await.unwrap();
        assert_eq!(decoded.id, 42);
        match decoded.payload {
            HelperRequest::Ping => (),
            _ => panic!("Expected Ping"),
        }
    }

    #[tokio::test]
    async fn test_daemon_protocol_roundtrip() {
        // Test DaemonRequest
        let req = IpcMessage {
            id: 101,
            payload: DaemonRequest::SubmitTask {
                instruction: "Click Safari".into(),
            },
        };
        let mut buffer = Vec::new();
        write_ipc_message(&mut buffer, &req).await.unwrap();
        let mut cursor = Cursor::new(buffer);
        let decoded: IpcMessage<DaemonRequest> = read_ipc_message(&mut cursor).await.unwrap();
        assert_eq!(decoded.id, 101);
        match decoded.payload {
            DaemonRequest::SubmitTask { instruction } => assert_eq!(instruction, "Click Safari"),
            _ => panic!("Expected SubmitTask"),
        }

        // Test DaemonResponse with Event
        let event = DaemonEvent::HudStateChanged {
            state: "Acting".into(),
            detail: "Clicking button".into(),
            daily_spend_usd: 0.12,
        };
        let res = IpcMessage {
            id: 102,
            payload: DaemonResponse::Event(event),
        };
        let mut buffer2 = Vec::new();
        write_ipc_message(&mut buffer2, &res).await.unwrap();
        let mut cursor2 = Cursor::new(buffer2);
        let decoded2: IpcMessage<DaemonResponse> = read_ipc_message(&mut cursor2).await.unwrap();
        assert_eq!(decoded2.id, 102);
        match decoded2.payload {
            DaemonResponse::Event(DaemonEvent::HudStateChanged {
                state,
                detail,
                daily_spend_usd,
            }) => {
                assert_eq!(state, "Acting");
                assert_eq!(detail, "Clicking button");
                assert!((daily_spend_usd - 0.12).abs() < 1e-6);
            }
            _ => panic!("Expected HudStateChanged event"),
        }
    }

    #[tokio::test]
    async fn test_openbot_daemon_ipc_roundtrip() {
        let req = IpcMessage {
            id: 201,
            payload: DaemonRequest::QueueTask {
                bot_id: Some("coder".into()),
                channel_id: Some("general".into()),
                instruction: "Write unit tests".into(),
                priority: 10,
            },
        };
        let mut buf = Vec::new();
        write_ipc_message(&mut buf, &req).await.unwrap();
        let mut cur = Cursor::new(buf);
        let decoded: IpcMessage<DaemonRequest> = read_ipc_message(&mut cur).await.unwrap();
        match decoded.payload {
            DaemonRequest::QueueTask {
                bot_id,
                channel_id,
                instruction,
                priority,
            } => {
                assert_eq!(bot_id.as_deref(), Some("coder"));
                assert_eq!(channel_id.as_deref(), Some("general"));
                assert_eq!(instruction, "Write unit tests");
                assert_eq!(priority, 10);
            }
            _ => panic!("Expected QueueTask"),
        }

        let evt = DaemonEvent::WheelHandoffRequested {
            task_id: "t-99".into(),
            bot_id: "muse".into(),
            blocker: "2FA code required".into(),
            instructions: "Complete 2FA in browser and click Hand Back to Bot".into(),
        };
        let res = IpcMessage {
            id: 202,
            payload: DaemonResponse::Event(evt),
        };
        let mut buf2 = Vec::new();
        write_ipc_message(&mut buf2, &res).await.unwrap();
        let mut cur2 = Cursor::new(buf2);
        let decoded2: IpcMessage<DaemonResponse> = read_ipc_message(&mut cur2).await.unwrap();
        match decoded2.payload {
            DaemonResponse::Event(DaemonEvent::WheelHandoffRequested {
                task_id,
                bot_id,
                blocker,
                ..
            }) => {
                assert_eq!(task_id, "t-99");
                assert_eq!(bot_id, "muse");
                assert_eq!(blocker, "2FA code required");
            }
            _ => panic!("Expected WheelHandoffRequested"),
        }
    }

    #[tokio::test]
    async fn test_muse_ipc_all_new_variants_roundtrip() {
        let requests = vec![
            DaemonRequest::ListActivity,
            DaemonRequest::RateActivity {
                id: "post-1".into(),
                rating: 5,
            },
            DaemonRequest::ListGoals,
            DaemonRequest::SaveGoal {
                op: "create".into(),
                slug: Some("learn-rust".into()),
                payload: serde_json::json!({"title": "Learn Rust"}),
            },
            DaemonRequest::ListSpaces,
            DaemonRequest::CreateSpace {
                slug: "budget".into(),
                title: Some("Budget".into()),
                kind: Some("static".into()),
                html: None,
            },
            DaemonRequest::SearchSkills {
                query: "goals".into(),
            },
            DaemonRequest::ReadRuntimeFile {
                bot_id: Some("muse".into()),
                file: "USER.md".into(),
            },
            DaemonRequest::SaveRuntimeFile {
                bot_id: Some("muse".into()),
                file: "USER.md".into(),
                content: "# USER.md\n".into(),
                expected_sha256: Some("abc123".into()),
            },
            DaemonRequest::ImportMemory {
                input: "- Prefers green tea".into(),
                apply: false,
                report_sha256: None,
            },
            DaemonRequest::ForgetPlan {
                bot_id: Some("muse".into()),
                request: "old address".into(),
            },
            DaemonRequest::ForgetConfirm {
                bot_id: Some("muse".into()),
                plan_id: "plan-1".into(),
                verification_query: None,
                confirmed: true,
            },
            DaemonRequest::UpdateDiscretion {
                chat_id: "slack-C123".into(),
                rules: vec!["Keep concise".into()],
                expected_sha256: Some("def456".into()),
            },
        ];

        for (idx, req) in requests.into_iter().enumerate() {
            let msg = IpcMessage {
                id: (idx + 300) as u64,
                payload: req,
            };
            let json_val = serde_json::to_value(&msg.payload).unwrap();
            assert!(json_val.get("method").is_some());

            let mut buf = Vec::new();
            write_ipc_message(&mut buf, &msg).await.unwrap();
            let mut cur = Cursor::new(buf);
            let decoded: IpcMessage<DaemonRequest> = read_ipc_message(&mut cur).await.unwrap();
            assert_eq!(decoded.id, (idx + 300) as u64);
            assert_eq!(serde_json::to_value(&decoded.payload).unwrap(), json_val);
        }

        let evt_resp = IpcMessage {
            id: 400,
            payload: DaemonResponse::Event(DaemonEvent::ActivityUpdated {
                reason: "feed_post_added".into(),
            }),
        };
        let mut buf = Vec::new();
        write_ipc_message(&mut buf, &evt_resp).await.unwrap();
        let mut cur = Cursor::new(buf);
        let decoded: IpcMessage<DaemonResponse> = read_ipc_message(&mut cur).await.unwrap();
        match decoded.payload {
            DaemonResponse::Event(DaemonEvent::ActivityUpdated { reason }) => {
                assert_eq!(reason, "feed_post_added");
            }
            _ => panic!("Expected ActivityUpdated"),
        }

        let payload_resp = IpcMessage {
            id: 401,
            payload: DaemonResponse::JsonPayload(serde_json::json!({ "ok": true, "count": 3 })),
        };
        let mut buf2 = Vec::new();
        write_ipc_message(&mut buf2, &payload_resp).await.unwrap();
        let mut cur2 = Cursor::new(buf2);
        let decoded2: IpcMessage<DaemonResponse> = read_ipc_message(&mut cur2).await.unwrap();
        match decoded2.payload {
            DaemonResponse::JsonPayload(v) => assert_eq!(v["count"], 3),
            _ => panic!("Expected JsonPayload"),
        }
    }

    #[tokio::test]
    async fn test_grokbot_ui_ipc_all_new_variants_roundtrip() {
        let requests = vec![
            DaemonRequest::GetBootstrapState,
            DaemonRequest::GetUiSettings,
            DaemonRequest::SaveUiSettings {
                ui: serde_json::json!({"theme": "dark", "accent_mode": "black"}),
            },
            DaemonRequest::SetBotUiPrefs {
                bot_id: "muse".into(),
                prefs: serde_json::json!({"pinned": true, "avatar_shape": "Hex"}),
            },
            DaemonRequest::ManageBotSection {
                op: "upsert".into(),
                id: Some("sec-1".into()),
                name: Some("Work".into()),
                collapsed: Some(false),
                sort_order: Some(1),
            },
            DaemonRequest::GetDirectChatHistory {
                bot_id: "muse".into(),
                before_seq: Some(50),
                limit: Some(40),
            },
            DaemonRequest::PostDirectChat {
                bot_id: "muse".into(),
                content: "Hello Grok Bot".into(),
                reply_to_id: Some("dm-1".into()),
                thread_id: Some("thr-1".into()),
            },
            DaemonRequest::SearchMessages {
                query: "invoice".into(),
                include_older: true,
                bot_id: Some("muse".into()),
            },
            DaemonRequest::MarkChatRead {
                chat_kind: "dm".into(),
                chat_id: "muse".into(),
            },
            DaemonRequest::ToggleMessageReaction {
                chat_kind: "dm".into(),
                message_id: "dm-2".into(),
                emoji: "👍".into(),
                actor: Some("user".into()),
            },
            DaemonRequest::SubmitSecretPrompt {
                request_id: "sec-req-1".into(),
                bot_id: "muse".into(),
                fields: vec![("password".into(), "hunter2".into())],
                save_to_vault: true,
                vault_label: Some("GitHub".into()),
                domain: Some("github.com".into()),
            },
            DaemonRequest::RespondQuestion {
                request_id: "q-1".into(),
                answers: vec!["Option A".into()],
                custom_text: None,
            },
            DaemonRequest::ManageCustomRule {
                op: "upsert".into(),
                rule: Some(serde_json::json!({"mode": "allow", "pattern": "git status"})),
                id: None,
            },
            DaemonRequest::ManageVault {
                op: "list".into(),
                item: None,
                id: None,
                reveal_confirmed: false,
            },
            DaemonRequest::ImportVaultCsv {
                csv: "name,url,username,password\n".into(),
                source_hint: Some("1Password".into()),
            },
            DaemonRequest::GetUsageBreakdown { days: Some(7) },
            DaemonRequest::TriggerRoutineTest {
                routine_id: "r-1".into(),
            },
            DaemonRequest::DeleteRoutine {
                routine_id: "r-1".into(),
            },
            DaemonRequest::ListRoutineRuns {
                routine_id: "r-1".into(),
                limit: Some(20),
            },
            DaemonRequest::ListLibraryPage {
                bot_id: "muse".into(),
                filter: "Pages".into(),
                cursor: Some(0),
                limit: Some(5),
            },
            DaemonRequest::ListCustomConnectors,
            DaemonRequest::SaveCustomConnector {
                op: "upsert".into(),
                connector: Some(serde_json::json!({"name": "Linear", "kind": "RemoteHttps"})),
                id: None,
            },
            DaemonRequest::CaptureComputerPreview {
                max_long_edge: Some(1280),
                quality: Some(75),
            },
            DaemonRequest::StartTeachTask {
                bot_id: "muse".into(),
            },
            DaemonRequest::StopTeachTask {
                save_skill_name: Some("daily-triage".into()),
            },
            DaemonRequest::CheckUpdates,
        ];

        for (idx, req) in requests.into_iter().enumerate() {
            let msg = IpcMessage {
                id: (idx + 500) as u64,
                payload: req,
            };
            let json_val = serde_json::to_value(&msg.payload).unwrap();
            assert!(json_val.get("method").is_some());

            let mut buf = Vec::new();
            write_ipc_message(&mut buf, &msg).await.unwrap();
            let mut cur = Cursor::new(buf);
            let decoded: IpcMessage<DaemonRequest> = read_ipc_message(&mut cur).await.unwrap();
            assert_eq!(serde_json::to_value(&decoded.payload).unwrap(), json_val);
        }

        let events = vec![
            DaemonEvent::BotUpdated {
                bot_id: "muse".into(),
            },
            DaemonEvent::BotStatusPhrase {
                bot_id: "muse".into(),
                phrase: Some("Checking mail…".into()),
                needs_attention: false,
            },
            DaemonEvent::DirectMessageAdded {
                bot_id: "muse".into(),
                message: serde_json::json!({"id": "dm-1", "content": "hi"}),
            },
            DaemonEvent::MessageCardUpdated {
                bot_id: "muse".into(),
                message_id: "dm-1".into(),
                card: serde_json::json!({"status": "approved"}),
            },
            DaemonEvent::MessageReactionUpdated {
                chat_kind: "dm".into(),
                chat_id: "muse".into(),
                message_id: "dm-1".into(),
                reactions: serde_json::json!([{"emoji": "👍", "count": 1}]),
            },
            DaemonEvent::RoutineUpdated {
                routine_id: Some("r-1".into()),
                bot_id: Some("muse".into()),
            },
            DaemonEvent::RoutineRunRecorded {
                routine_id: "r-1".into(),
                run: serde_json::json!({"status": "queued"}),
            },
            DaemonEvent::TaskUpdated {
                task: serde_json::json!({"id": "t-1"}),
            },
            DaemonEvent::ApprovalExpired {
                request_id: "req-1".into(),
            },
            DaemonEvent::SecretPromptRequested {
                request_id: "sec-1".into(),
                bot_id: "muse".into(),
                title: "Sign in".into(),
                domain: Some("github.com".into()),
                fields: vec!["username".into(), "password".into()],
            },
            DaemonEvent::QuestionRequested {
                request_id: "q-1".into(),
                bot_id: "muse".into(),
                prompt: "Pick one".into(),
                options: vec!["A".into(), "B".into()],
                allow_multiple: false,
            },
            DaemonEvent::OrderApprovalRequested {
                request_id: "ord-1".into(),
                bot_id: "muse".into(),
                merchant: "Acme".into(),
                item_title: "Cable".into(),
                amount_usd: 19.99,
            },
            DaemonEvent::BudgetUpdated {
                daily_spent_usd: 1.24,
                daily_cap_usd: 5.0,
            },
            DaemonEvent::ComputerFrameReady {
                width: 1280,
                height: 720,
                jpeg_base64: "AAAA".into(),
                timestamp_ms: 123456,
            },
            DaemonEvent::NotificationAlert {
                id: "n-1".into(),
                bot_id: Some("muse".into()),
                level: "warning".into(),
                title: "Token expired".into(),
                body: "Reconnect Gmail".into(),
                request_id: None,
            },
        ];

        for (idx, evt) in events.into_iter().enumerate() {
            let msg = IpcMessage {
                id: (idx + 700) as u64,
                payload: DaemonResponse::Event(evt),
            };
            let json_val = serde_json::to_value(&msg.payload).unwrap();
            let mut buf = Vec::new();
            write_ipc_message(&mut buf, &msg).await.unwrap();
            let mut cur = Cursor::new(buf);
            let decoded: IpcMessage<DaemonResponse> = read_ipc_message(&mut cur).await.unwrap();
            assert_eq!(serde_json::to_value(&decoded.payload).unwrap(), json_val);
        }
    }
}
