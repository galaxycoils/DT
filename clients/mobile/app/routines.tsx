import React, { useState } from "react";
import { View, Text, Pressable, ScrollView } from "react-native";
import { darkTokens } from "../lib/tokens";
import { mobileV1Endpoint } from "../lib/api";

const DEFAULT_ROUTINES = [
  {
    id: "morning-brief",
    name: "Morning Research & Calendar Brief",
    botId: "muse",
    schedule: "09:00 · America/Los_Angeles",
    quietHours: "22:00–07:00",
    lastOutcome: "queued",
  },
  {
    id: "repo-health",
    name: "Worktree & Safety Invariant Patrol",
    botId: "guardian",
    schedule: "14:00 · UTC",
    quietHours: "23:00–06:00",
    lastOutcome: "skipped_quiet_hours",
  },
];

export default function RoutinesScreen() {
  const [outcomes, setOutcomes] = useState<Record<string, string>>({});
  const endpoint = mobileV1Endpoint("", "/routines");

  function handleTrigger(id: string) {
    setOutcomes((prev) => ({
      ...prev,
      [id]: prev[id] === "queued" ? "duplicate" : "queued",
    }));
  }

  return (
    <ScrollView
      style={{ flex: 1, backgroundColor: darkTokens.background }}
      contentContainerStyle={{ padding: 16, gap: 12 }}
    >
      <Text style={{ color: darkTokens.foreground, fontSize: 20, fontWeight: "700" }}>
        Routines & Run History
      </Text>
      <Text style={{ color: darkTokens.mutedForeground, fontSize: 12 }}>
        Endpoint: {endpoint} · Idempotent delivery_id deduplication
      </Text>

      {DEFAULT_ROUTINES.map((rtn) => {
        const status = outcomes[rtn.id] || rtn.lastOutcome;
        return (
          <View
            key={rtn.id}
            style={{
              padding: 15,
              backgroundColor: darkTokens.card,
              borderRadius: 16,
              gap: 8,
            }}
          >
            <View style={{ flexDirection: "row", justifyContent: "space-between", alignItems: "center" }}>
              <Text style={{ color: darkTokens.foreground, fontSize: 15, fontWeight: "600" }}>
                {rtn.name}
              </Text>
              <Text style={{ color: darkTokens.link, fontSize: 12, fontWeight: "600" }}>
                @{rtn.botId}
              </Text>
            </View>
            <Text style={{ color: darkTokens.mutedForeground, fontSize: 12 }}>
              Schedule: {rtn.schedule} · Quiet hours: {rtn.quietHours}
            </Text>
            <View style={{ flexDirection: "row", justifyContent: "space-between", alignItems: "center", marginTop: 4 }}>
              <Text style={{ color: darkTokens.faintForeground, fontSize: 12 }}>
                Outcome: {status}
              </Text>
              <Pressable
                onPress={() => handleTrigger(rtn.id)}
                style={{
                  paddingHorizontal: 12,
                  paddingVertical: 6,
                  borderRadius: 10,
                  backgroundColor: darkTokens.dialog,
                }}
              >
                <Text style={{ color: darkTokens.foreground, fontSize: 12, fontWeight: "600" }}>
                  Trigger Now
                </Text>
              </Pressable>
            </View>
          </View>
        );
      })}
    </ScrollView>
  );
}
