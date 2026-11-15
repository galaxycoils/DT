use crate::workspace_tools::WorkspaceToolCall;
use gm_core::types::AxSnapshot;
use gm_safety::ActionTier;
use serde::{Deserialize, Serialize};

/// Request emitted when an agent encounters a blocking wall (CAPTCHA, 2FA/MFA, login prompt)
/// and hands control back to a human operator ("Take the Wheel").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WheelHandoffRequest {
    pub task_id: String,
    pub bot_id: Option<String>,
    pub step: u32,
    pub blocker: String,
    pub instructions: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalOpKind {
    Create,
    Update,
    Complete,
    Abandon,
    Delete,
    CreateSubgoal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpaceOpKind {
    Create,
    Edit,
    Publish,
    Unpublish,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DirectiveKind {
    Reaction { emoji: String },
    Options { labels: Vec<String> },
}

/// Extended OpenBot step actions emitted by the model alongside standard GUI and MCP actions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum OpenBotStepAction {
    TakeTheWheel {
        blocker: String,
        instructions: String,
    },
    FillSecret {
        token: Option<String>,
        secret_ref: String,
    },
    WorkspaceTool(WorkspaceToolCall),
    DelegateToBot {
        target_bot_id: String,
        goal: String,
        #[serde(default)]
        artifacts: Vec<String>,
    },
    SkillSearch {
        query: String,
    },
    LoadSkill {
        name: String,
    },
    ConnectorSearch {
        query: String,
    },
    LoadConnector {
        id: String,
    },
    MemorySearch {
        query: String,
        #[serde(default)]
        limit: Option<u32>,
    },
    MemoryExplain {
        target: String,
    },
    ForgetPlan {
        request: String,
    },
    ForgetConfirm {
        plan_id: String,
    },
    CreateCustomSkill {
        name: String,
        title: String,
        description: String,
        body_markdown: String,
    },
    GoalOp {
        op: GoalOpKind,
        #[serde(default)]
        slug: Option<String>,
        #[serde(default)]
        payload: serde_json::Value,
    },
    SpaceOp {
        op: SpaceOpKind,
        slug: String,
        #[serde(default)]
        payload: serde_json::Value,
    },
    UpdateDiscretion {
        chat_id: String,
        rules: String,
        loosens: bool,
    },
    EmitDirective {
        directive: DirectiveKind,
    },
}

/// Classify an [`OpenBotStepAction`] into an enforcement [`ActionTier`].
/// Anything unmapped or destructive fails closed to [`ActionTier::Irreversible`].
pub fn classify_openbot_step_tier(a: &OpenBotStepAction) -> ActionTier {
    match a {
        OpenBotStepAction::SkillSearch { .. }
        | OpenBotStepAction::LoadSkill { .. }
        | OpenBotStepAction::ConnectorSearch { .. }
        | OpenBotStepAction::LoadConnector { .. }
        | OpenBotStepAction::MemorySearch { .. }
        | OpenBotStepAction::MemoryExplain { .. }
        | OpenBotStepAction::ForgetPlan { .. }
        | OpenBotStepAction::EmitDirective { .. }
        | OpenBotStepAction::TakeTheWheel { .. }
        | OpenBotStepAction::WorkspaceTool(WorkspaceToolCall::ReadFile { .. })
        | OpenBotStepAction::WorkspaceTool(WorkspaceToolCall::ListFiles { .. }) => ActionTier::Read,

        OpenBotStepAction::CreateCustomSkill { .. }
        | OpenBotStepAction::DelegateToBot { .. }
        | OpenBotStepAction::WorkspaceTool(WorkspaceToolCall::WriteFile { .. })
        | OpenBotStepAction::WorkspaceTool(WorkspaceToolCall::Shell { .. }) => {
            ActionTier::Reversible
        }

        OpenBotStepAction::GoalOp { op, .. } if !matches!(op, GoalOpKind::Delete) => {
            ActionTier::Reversible
        }
        OpenBotStepAction::SpaceOp { op, .. }
            if !matches!(op, SpaceOpKind::Publish | SpaceOpKind::Delete) =>
        {
            ActionTier::Reversible
        }
        OpenBotStepAction::UpdateDiscretion { loosens: false, .. } => ActionTier::Reversible,

        // ForgetConfirm, FillSecret, GoalOp::Delete, SpaceOp::{Publish, Delete},
        // UpdateDiscretion { loosens: true }, and any future unmapped variant fail closed.
        _ => ActionTier::Irreversible,
    }
}

/// Detects CAPTCHA, Turnstile, MFA/2FA, or SSO login walls in a sanitized AX snapshot.
pub struct BlockerDetector;

impl BlockerDetector {
    pub fn detect_in_snapshot(snapshot: &AxSnapshot) -> Option<String> {
        let title_lower = snapshot.window_title.to_lowercase();
        let captcha_keywords = [
            "verify you are human",
            "cloudflare turnstile",
            "recaptcha",
            "hcaptcha",
            "security challenge",
        ];
        for kw in captcha_keywords {
            if title_lower.contains(kw) {
                return Some(format!(
                    "CAPTCHA / human verification wall detected in window '{}'",
                    snapshot.window_title
                ));
            }
        }

        for el in &snapshot.elements {
            let lbl = el.label.as_deref().unwrap_or("").to_lowercase();
            let val = el.value.as_deref().unwrap_or("").to_lowercase();
            let combined = format!("{lbl} {val}");

            for kw in captcha_keywords {
                if combined.contains(kw) {
                    return Some(format!(
                        "CAPTCHA challenge element ('{}') requires human interaction",
                        el.label.as_deref().unwrap_or(&el.token)
                    ));
                }
            }

            let mfa_keywords = [
                "two-factor authentication",
                "enter 6-digit code",
                "authenticator code",
                "hardware security key",
                "touch id to continue",
            ];
            for kw in mfa_keywords {
                if combined.contains(kw) {
                    return Some(format!(
                        "MFA / 2FA verification wall ('{}') requires human interaction",
                        el.label.as_deref().unwrap_or(&el.token)
                    ));
                }
            }
        }

        None
    }
}

/// Parses an OpenBot-specific JSON action (`TakeTheWheel`, `FillSecret`, `WorkspaceTool`,
/// `DelegateToBot`, skills/connectors, memory/forget, goals, spaces, discretion, directives)
/// from the raw model completion string before falling back to `ComputerAction`.
pub fn try_parse_openbot_step_action(raw: &str) -> Option<OpenBotStepAction> {
    let trimmed = raw.trim();
    let json_str = if let Some(start) = trimmed.find('{')
        && let Some(end) = trimmed.rfind('}')
        && end >= start
    {
        &trimmed[start..=end]
    } else {
        trimmed
    };

    let val: serde_json::Value = serde_json::from_str(json_str).ok()?;
    let Some(action_name) = val.get("action").and_then(|v| v.as_str()) else {
        return serde_json::from_value(val).ok();
    };
    let params = val.get("params").cloned().unwrap_or(serde_json::json!({}));

    match action_name {
        "TakeTheWheel" | "take_the_wheel" => {
            let blocker = params
                .get("blocker")
                .and_then(|v| v.as_str())
                .unwrap_or("Human intervention required")
                .to_string();
            let instructions = params
                .get("instructions")
                .and_then(|v| v.as_str())
                .unwrap_or("Please complete the blocking step and click 'Hand Back to Bot'")
                .to_string();
            Some(OpenBotStepAction::TakeTheWheel {
                blocker,
                instructions,
            })
        }
        "FillSecret" | "fill_secret" => {
            let secret_ref = params.get("secret_ref")?.as_str()?.to_string();
            let token = params
                .get("token")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            Some(OpenBotStepAction::FillSecret { token, secret_ref })
        }
        "WorkspaceReadFile" | "workspace_read_file" | "read_file" => {
            let path = params.get("path")?.as_str()?.to_string();
            Some(OpenBotStepAction::WorkspaceTool(
                WorkspaceToolCall::ReadFile { path },
            ))
        }
        "WorkspaceWriteFile" | "workspace_write_file" | "write_file" => {
            let path = params.get("path")?.as_str()?.to_string();
            let content = params
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some(OpenBotStepAction::WorkspaceTool(
                WorkspaceToolCall::WriteFile { path, content },
            ))
        }
        "WorkspaceListFiles" | "workspace_list_files" | "list_files" => {
            let path = params
                .get("path")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            Some(OpenBotStepAction::WorkspaceTool(
                WorkspaceToolCall::ListFiles { path },
            ))
        }
        "WorkspaceShell" | "workspace_shell" | "shell" => {
            let command = params.get("command")?.as_str()?.to_string();
            let timeout_ms = params.get("timeout_ms").and_then(|v| v.as_u64());
            Some(OpenBotStepAction::WorkspaceTool(WorkspaceToolCall::Shell {
                command,
                timeout_ms,
            }))
        }
        "DelegateToBot" | "delegate_to_bot" => {
            let target_bot_id = params
                .get("target_bot_id")
                .or_else(|| params.get("bot_id"))?
                .as_str()?
                .to_string();
            let goal = params.get("goal")?.as_str()?.to_string();
            let artifacts = params
                .get("artifacts")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|x| x.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            Some(OpenBotStepAction::DelegateToBot {
                target_bot_id,
                goal,
                artifacts,
            })
        }
        "SkillSearch" | "skill_search" => {
            let query = params
                .get("query")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some(OpenBotStepAction::SkillSearch { query })
        }
        "LoadSkill" | "load_skill" => {
            let name = params.get("name")?.as_str()?.to_string();
            Some(OpenBotStepAction::LoadSkill { name })
        }
        "ConnectorSearch" | "connector_search" => {
            let query = params
                .get("query")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some(OpenBotStepAction::ConnectorSearch { query })
        }
        "LoadConnector" | "load_connector" => {
            let id = params.get("id")?.as_str()?.to_string();
            Some(OpenBotStepAction::LoadConnector { id })
        }
        "MemorySearch" | "memory_search" => {
            let query = params.get("query")?.as_str()?.to_string();
            let limit = params
                .get("limit")
                .and_then(|v| v.as_u64())
                .and_then(|n| u32::try_from(n).ok());
            Some(OpenBotStepAction::MemorySearch { query, limit })
        }
        "MemoryExplain" | "memory_explain" => {
            let target = params.get("target")?.as_str()?.to_string();
            Some(OpenBotStepAction::MemoryExplain { target })
        }
        "ForgetPlan" | "forget_plan" => {
            let request = params.get("request")?.as_str()?.to_string();
            Some(OpenBotStepAction::ForgetPlan { request })
        }
        "ForgetConfirm" | "forget_confirm" => {
            let plan_id = params.get("plan_id")?.as_str()?.to_string();
            Some(OpenBotStepAction::ForgetConfirm { plan_id })
        }
        "CreateCustomSkill" | "create_custom_skill" => {
            let name = params.get("name")?.as_str()?.to_string();
            let title = params.get("title")?.as_str()?.to_string();
            let description = params.get("description")?.as_str()?.to_string();
            let body_markdown = params.get("body_markdown")?.as_str()?.to_string();
            Some(OpenBotStepAction::CreateCustomSkill {
                name,
                title,
                description,
                body_markdown,
            })
        }
        "GoalOp" | "goal_op" => {
            let op: GoalOpKind = serde_json::from_value(params.get("op")?.clone()).ok()?;
            let slug = params
                .get("slug")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let payload = params
                .get("payload")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            Some(OpenBotStepAction::GoalOp { op, slug, payload })
        }
        "SpaceOp" | "space_op" => {
            let op: SpaceOpKind = serde_json::from_value(params.get("op")?.clone()).ok()?;
            let slug = params.get("slug")?.as_str()?.to_string();
            let payload = params
                .get("payload")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            Some(OpenBotStepAction::SpaceOp { op, slug, payload })
        }
        "UpdateDiscretion" | "update_discretion" => {
            let chat_id = params.get("chat_id")?.as_str()?.to_string();
            let rules = params.get("rules")?.as_str()?.to_string();
            // Fail closed to `loosens: true` (Irreversible) if the model omits `loosens`.
            let loosens = params
                .get("loosens")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            Some(OpenBotStepAction::UpdateDiscretion {
                chat_id,
                rules,
                loosens,
            })
        }
        "EmitDirective" | "emit_directive" => {
            let directive = if let Some(d) = params.get("directive") {
                serde_json::from_value::<DirectiveKind>(d.clone()).ok()?
            } else if let Some(emoji) = params.get("emoji").and_then(|v| v.as_str()) {
                DirectiveKind::Reaction {
                    emoji: emoji.to_string(),
                }
            } else if let Some(labels) = params.get("labels").and_then(|v| v.as_array()) {
                let list = labels
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect();
                DirectiveKind::Options { labels: list }
            } else {
                serde_json::from_value::<DirectiveKind>(params).ok()?
            };
            Some(OpenBotStepAction::EmitDirective { directive })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gm_core::types::{AxElement, Rect};

    #[test]
    fn test_blocker_detector_finds_captcha_and_mfa() {
        let snap = AxSnapshot {
            window_id: 1,
            app_name: "Safari".into(),
            window_title: "Sign In".into(),
            bounds: Rect::new(0.0, 0.0, 800.0, 600.0),
            elements: vec![AxElement {
                token: "el_1".into(),
                role: "AXStaticText".into(),
                label: Some("Enter 6-digit code from your authenticator".into()),
                value: None,
                frame: Rect::new(10.0, 10.0, 200.0, 30.0),
                is_enabled: true,
                is_secure: false,
            }],
            timestamp_ms: 1,
        };
        let detected = BlockerDetector::detect_in_snapshot(&snap);
        assert!(detected.is_some());
        assert!(detected.unwrap().contains("MFA / 2FA"));
    }

    #[test]
    fn test_parse_openbot_step_actions() {
        let w = try_parse_openbot_step_action(
            r#"{"action": "TakeTheWheel", "params": {"blocker": "SSO MFA", "instructions": "Approve Duo push"}}"#,
        )
        .unwrap();
        match w {
            OpenBotStepAction::TakeTheWheel {
                blocker,
                instructions,
            } => {
                assert_eq!(blocker, "SSO MFA");
                assert_eq!(instructions, "Approve Duo push");
            }
            _ => panic!("Expected TakeTheWheel"),
        }

        let s = try_parse_openbot_step_action(
            r#"{"action": "FillSecret", "params": {"token": "pwd_box", "secret_ref": "secret://keychain/corp_sso"}}"#,
        )
        .unwrap();
        match s {
            OpenBotStepAction::FillSecret { token, secret_ref } => {
                assert_eq!(token.as_deref(), Some("pwd_box"));
                assert_eq!(secret_ref, "secret://keychain/corp_sso");
            }
            _ => panic!("Expected FillSecret"),
        }
    }

    #[test]
    fn every_openbot_step_action_roundtrips_serde_and_classifies_tier() {
        let cases: Vec<(OpenBotStepAction, ActionTier)> = vec![
            (
                OpenBotStepAction::TakeTheWheel {
                    blocker: "CAPTCHA".into(),
                    instructions: "Solve it".into(),
                },
                ActionTier::Read,
            ),
            (
                OpenBotStepAction::FillSecret {
                    token: Some("pw".into()),
                    secret_ref: "secret://keychain/gh".into(),
                },
                ActionTier::Irreversible,
            ),
            (
                OpenBotStepAction::WorkspaceTool(WorkspaceToolCall::ReadFile {
                    path: "notes.md".into(),
                }),
                ActionTier::Read,
            ),
            (
                OpenBotStepAction::WorkspaceTool(WorkspaceToolCall::ListFiles {
                    path: Some(".".into()),
                }),
                ActionTier::Read,
            ),
            (
                OpenBotStepAction::WorkspaceTool(WorkspaceToolCall::WriteFile {
                    path: "notes.md".into(),
                    content: "hi".into(),
                }),
                ActionTier::Reversible,
            ),
            (
                OpenBotStepAction::WorkspaceTool(WorkspaceToolCall::Shell {
                    command: "echo ok".into(),
                    timeout_ms: Some(1000),
                }),
                ActionTier::Reversible,
            ),
            (
                OpenBotStepAction::DelegateToBot {
                    target_bot_id: "researcher".into(),
                    goal: "Find X".into(),
                    artifacts: vec!["report.md".into()],
                },
                ActionTier::Reversible,
            ),
            (
                OpenBotStepAction::SkillSearch {
                    query: "goals".into(),
                },
                ActionTier::Read,
            ),
            (
                OpenBotStepAction::LoadSkill {
                    name: "forget".into(),
                },
                ActionTier::Read,
            ),
            (
                OpenBotStepAction::ConnectorSearch {
                    query: "linear".into(),
                },
                ActionTier::Read,
            ),
            (
                OpenBotStepAction::LoadConnector {
                    id: "custom.linear".into(),
                },
                ActionTier::Read,
            ),
            (
                OpenBotStepAction::MemorySearch {
                    query: "coffee".into(),
                    limit: Some(5),
                },
                ActionTier::Read,
            ),
            (
                OpenBotStepAction::MemoryExplain {
                    target: "claim-1".into(),
                },
                ActionTier::Read,
            ),
            (
                OpenBotStepAction::ForgetPlan {
                    request: "remove secret project".into(),
                },
                ActionTier::Read,
            ),
            (
                OpenBotStepAction::ForgetConfirm {
                    plan_id: "plan-123".into(),
                },
                ActionTier::Irreversible,
            ),
            (
                OpenBotStepAction::CreateCustomSkill {
                    name: "daily-brief".into(),
                    title: "Daily Brief".into(),
                    description: "Summarize morning items".into(),
                    body_markdown: "# Steps\n1. Check goals\n".into(),
                },
                ActionTier::Reversible,
            ),
            (
                OpenBotStepAction::GoalOp {
                    op: GoalOpKind::Create,
                    slug: Some("learn-rust".into()),
                    payload: serde_json::json!({"title": "Learn Rust"}),
                },
                ActionTier::Reversible,
            ),
            (
                OpenBotStepAction::GoalOp {
                    op: GoalOpKind::CreateSubgoal,
                    slug: Some("learn-rust".into()),
                    payload: serde_json::json!({"title": "Async traits"}),
                },
                ActionTier::Reversible,
            ),
            (
                OpenBotStepAction::GoalOp {
                    op: GoalOpKind::Delete,
                    slug: Some("learn-rust".into()),
                    payload: serde_json::Value::Null,
                },
                ActionTier::Irreversible,
            ),
            (
                OpenBotStepAction::SpaceOp {
                    op: SpaceOpKind::Create,
                    slug: "reading-list".into(),
                    payload: serde_json::json!({"kind": "static"}),
                },
                ActionTier::Reversible,
            ),
            (
                OpenBotStepAction::SpaceOp {
                    op: SpaceOpKind::Publish,
                    slug: "reading-list".into(),
                    payload: serde_json::Value::Null,
                },
                ActionTier::Irreversible,
            ),
            (
                OpenBotStepAction::SpaceOp {
                    op: SpaceOpKind::Delete,
                    slug: "reading-list".into(),
                    payload: serde_json::Value::Null,
                },
                ActionTier::Irreversible,
            ),
            (
                OpenBotStepAction::UpdateDiscretion {
                    chat_id: "whatsapp:123".into(),
                    rules: "Never share address".into(),
                    loosens: false,
                },
                ActionTier::Reversible,
            ),
            (
                OpenBotStepAction::UpdateDiscretion {
                    chat_id: "whatsapp:123".into(),
                    rules: "Auto-reply to strangers".into(),
                    loosens: true,
                },
                ActionTier::Irreversible,
            ),
            (
                OpenBotStepAction::EmitDirective {
                    directive: DirectiveKind::Options {
                        labels: vec!["Approve".into(), "Cancel".into()],
                    },
                },
                ActionTier::Read,
            ),
            (
                OpenBotStepAction::EmitDirective {
                    directive: DirectiveKind::Reaction {
                        emoji: "👍".into()
                    },
                },
                ActionTier::Read,
            ),
        ];

        for (action, expected_tier) in cases {
            assert_eq!(
                classify_openbot_step_tier(&action),
                expected_tier,
                "Tier mismatch for {action:?}"
            );
            let serialized = serde_json::to_string(&action).unwrap();
            let decoded: OpenBotStepAction = serde_json::from_str(&serialized).unwrap();
            assert_eq!(decoded, action, "Serde round-trip failed for {action:?}");
            let via_parser = try_parse_openbot_step_action(&serialized).unwrap();
            assert_eq!(via_parser, action);
        }

        // Also verify action/params envelope parsing for new Muse variants.
        assert_eq!(
            try_parse_openbot_step_action(
                r#"{"action":"SkillSearch","params":{"query":"memory"}}"#
            ),
            Some(OpenBotStepAction::SkillSearch {
                query: "memory".into()
            })
        );
        assert_eq!(
            try_parse_openbot_step_action(
                r#"{"action":"GoalOp","params":{"op":"create_subgoal","slug":"g1","payload":{"title":"Sub"}}}"#
            ),
            Some(OpenBotStepAction::GoalOp {
                op: GoalOpKind::CreateSubgoal,
                slug: Some("g1".into()),
                payload: serde_json::json!({"title": "Sub"})
            })
        );
        assert_eq!(
            try_parse_openbot_step_action(
                r#"{"action":"EmitDirective","params":{"directive":{"reaction":{"emoji":"🎉"}}}}"#
            ),
            Some(OpenBotStepAction::EmitDirective {
                directive: DirectiveKind::Reaction {
                    emoji: "🎉".into()
                }
            })
        );
    }
}
