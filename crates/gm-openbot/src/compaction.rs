use gm_llm::ChatMessage;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionReport {
    pub compacted: bool,
    pub estimated_tokens_before: usize,
    pub estimated_tokens_after: usize,
    pub summarized_messages_count: usize,
}

/// Automated context-window monitor & thread compactor (`nightly-labs/openbot` parity).
/// Compacts older conversation turns into a structured `[Compacted Session Summary]`
/// before long threads exhaust the model's token limit.
#[derive(Debug, Clone)]
pub struct ContextCompactor {
    pub max_context_tokens: usize,
    pub trigger_ratio: f32,
    pub keep_recent_turns: usize,
}

impl Default for ContextCompactor {
    fn default() -> Self {
        Self {
            max_context_tokens: 24_000,
            trigger_ratio: 0.80,
            keep_recent_turns: 4,
        }
    }
}

impl ContextCompactor {
    pub fn new(max_context_tokens: usize, trigger_ratio: f32, keep_recent_turns: usize) -> Self {
        Self {
            max_context_tokens: max_context_tokens.max(256),
            trigger_ratio: trigger_ratio.clamp(0.20, 0.98),
            keep_recent_turns: keep_recent_turns.max(1),
        }
    }

    pub fn estimate_tokens(messages: &[ChatMessage]) -> usize {
        messages
            .iter()
            .map(|m| {
                let text_tokens = (m.content.len() / 4).max(4);
                let img_tokens = m.images.len() * 768;
                text_tokens + img_tokens
            })
            .sum()
    }

    pub fn threshold_tokens(&self) -> usize {
        ((self.max_context_tokens as f32) * self.trigger_ratio) as usize
    }

    pub fn threshold_tokens_excluding_system(&self, system_tokens: usize) -> usize {
        ((self.max_context_tokens.saturating_sub(system_tokens) as f32) * self.trigger_ratio)
            as usize
    }

    /// Compact `messages` in-place if non-system tokens exceed
    /// `(max_context_tokens - system_tokens) * trigger_ratio`.
    /// Preserves leading `system` messages and the last `keep_recent_turns` messages verbatim.
    pub fn compact_if_needed(&self, messages: &mut Vec<ChatMessage>) -> CompactionReport {
        let leading_system_count = messages.iter().take_while(|m| m.role == "system").count();
        let system_tokens = Self::estimate_tokens(&messages[..leading_system_count]);
        let before = Self::estimate_tokens(messages);
        let conversation_tokens = before.saturating_sub(system_tokens);
        if conversation_tokens <= self.threshold_tokens_excluding_system(system_tokens)
            || messages.len().saturating_sub(leading_system_count) <= self.keep_recent_turns
        {
            return CompactionReport {
                compacted: false,
                estimated_tokens_before: before,
                estimated_tokens_after: before,
                summarized_messages_count: 0,
            };
        }

        let mut system_msgs = Vec::new();
        let mut non_system = Vec::new();
        for msg in messages.drain(..) {
            if msg.role == "system" && non_system.is_empty() {
                system_msgs.push(msg);
            } else {
                non_system.push(msg);
            }
        }

        if non_system.len() <= self.keep_recent_turns {
            messages.extend(system_msgs);
            messages.extend(non_system);
            return CompactionReport {
                compacted: false,
                estimated_tokens_before: before,
                estimated_tokens_after: before,
                summarized_messages_count: 0,
            };
        }

        let split_idx = non_system.len() - self.keep_recent_turns;
        let to_summarize = &non_system[..split_idx];
        let recent = &non_system[split_idx..];

        let mut summary_lines = Vec::new();
        summary_lines.push(format!(
            "[Compacted Session Summary — {} earlier messages condensed to preserve context window]",
            to_summarize.len()
        ));
        for (idx, m) in to_summarize.iter().enumerate() {
            let snippet: String = m.content.chars().take(160).collect();
            let clean = snippet.replace('\n', " ");
            summary_lines.push(format!("- Turn {}: [{}] {}", idx + 1, m.role, clean));
        }

        let checkpoint_msg = ChatMessage::text(
            "user",
            gm_safety::format_untrusted_context(
                &summary_lines.join("\n"),
                "compacted_session_history",
            ),
        );

        messages.extend(system_msgs);
        messages.push(checkpoint_msg);
        messages.extend_from_slice(recent);

        let after = Self::estimate_tokens(messages);
        CompactionReport {
            compacted: true,
            estimated_tokens_before: before,
            estimated_tokens_after: after,
            summarized_messages_count: split_idx,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_system_prompt_with_short_turns_does_not_compact() {
        let compactor = ContextCompactor::new(24_000, 0.80, 1);
        let mut msgs = vec![
            ChatMessage::text("system", "X".repeat(80_000)), // ~20,000 tokens
            ChatMessage::text("system", "[Sat 2026-11-14] [model=muse-spark]"),
            ChatMessage::text("user", "Hey, what is on my calendar today?"),
            ChatMessage::text("assistant", "Checking your calendar now."),
        ];

        let report = compactor.compact_if_needed(&mut msgs);
        assert!(!report.compacted);
        assert_eq!(report.summarized_messages_count, 0);
        assert_eq!(msgs.len(), 4);
    }

    #[test]
    fn large_system_prompt_compacts_when_conversation_grows() {
        let compactor = ContextCompactor::new(24_000, 0.80, 1);
        // Remaining budget after ~20,004 system tokens is ~3,996 * 0.80 = ~3,196 tokens.
        // Push ~3,600 tokens of conversation across 5 non-system messages.
        let mut msgs = vec![
            ChatMessage::text("system", "X".repeat(80_000)),
            ChatMessage::text("system", "[Sat 2026-11-14] [model=muse-spark]"),
            ChatMessage::text("user", "A".repeat(4_800)),
            ChatMessage::text("assistant", "B".repeat(4_800)),
            ChatMessage::text("user", "C".repeat(4_800)),
            ChatMessage::text("assistant", "Recent reply"),
        ];

        let report = compactor.compact_if_needed(&mut msgs);
        assert!(report.compacted);
        assert_eq!(report.summarized_messages_count, 3);
        assert_eq!(msgs[0].role, "system");
        assert_eq!(msgs[1].role, "system");
        assert!(msgs[2].content.contains("[Compacted Session Summary"));
        assert_eq!(msgs.last().unwrap().content, "Recent reply");
    }
}
