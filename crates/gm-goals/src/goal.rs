pub use gm_core::goal::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v0_fixture_deserializes_and_normalizes_paused_status() {
        let raw = include_str!("../tests/fixtures/goal_v0.json");
        let goals = Goal::list_from_json_normalized(raw).unwrap();
        assert_eq!(goals.len(), 2);

        let paused_legacy = &goals[0];
        assert_eq!(paused_legacy.status, GoalStatus::Active);
        assert_eq!(
            paused_legacy.pause_notes.as_deref(),
            Some("migrated from Paused")
        );
        assert_eq!(paused_legacy.slug, "run-a-half-marathon");
        assert!(paused_legacy.parent_goal_id.is_none());

        let active_legacy = &goals[1];
        assert_eq!(active_legacy.status, GoalStatus::Active);
        assert_eq!(active_legacy.pause_notes, None);
        assert_eq!(active_legacy.slug, "learn-conversational-japanese");
    }
}
