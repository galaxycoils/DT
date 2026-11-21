import React from "react";
import { View, Text, Pressable, ScrollView } from "react-native";
import { Link } from "expo-router";
import { darkTokens, layoutMetrics } from "../lib/tokens";

const COWORKERS = [
  { id: "muse", name: "Muse", role: "Primary Personal Agent", status: "Ready · 0 pending approvals", color: darkTokens.unread, unread: true },
  { id: "researcher", name: "Researcher", role: "Wide Research & Citations", status: "Provenance store synced", color: darkTokens.workingDot, unread: false },
  { id: "coder", name: "Coder", role: "Isolated Worktree & Patch", status: "VM sandbox idle", color: darkTokens.warning, unread: false },
  { id: "guardian", name: "Guardian", role: "Safety & Discretion Audit", status: "Zero policy alerts", color: darkTokens.success, unread: false },
];

export default function InboxScreen() {
  return (
    <ScrollView
      style={{ flex: 1, backgroundColor: darkTokens.background }}
      contentContainerStyle={{ padding: 16, gap: 14 }}
    >
      <View
        style={{
          flexDirection: "row",
          justifyContent: "space-between",
          alignItems: "center",
          paddingBottom: 6,
        }}
      >
        <View style={{ flexDirection: "row", alignItems: "center", gap: 8 }}>
          <View
            style={{
              width: 9,
              height: 9,
              borderRadius: 9,
              backgroundColor: darkTokens.workingDot,
            }}
          />
          <Text style={{ color: darkTokens.foreground, fontSize: 22, fontWeight: "700" }}>
            DT · Workspace
          </Text>
        </View>
        <View
          style={{
            paddingHorizontal: 10,
            paddingVertical: 4,
            borderRadius: 999,
            backgroundColor: darkTokens.card,
          }}
        >
          <Text style={{ color: darkTokens.mutedForeground, fontSize: 12 }}>
            v1.0.0 · SecureStore
          </Text>
        </View>
      </View>

      <Text
        style={{
          color: darkTokens.faintForeground,
          fontSize: 11,
          fontWeight: "600",
          letterSpacing: 0.6,
          textTransform: "uppercase",
        }}
      >
        Coworker Roster
      </Text>

      {COWORKERS.map((bot) => (
        <Link key={bot.id} href="/thread" asChild>
          <Pressable
            style={{
              padding: 14,
              backgroundColor: darkTokens.card,
              borderRadius: 16,
              flexDirection: "row",
              alignItems: "center",
              gap: 12,
            }}
          >
            <View
              style={{
                width: 38,
                height: 38,
                borderRadius: 12,
                backgroundColor: bot.color,
                alignItems: "center",
                justifyContent: "center",
              }}
            >
              <Text style={{ color: "#121212", fontWeight: "800", fontSize: 13 }}>
                ••
              </Text>
            </View>
            <View style={{ flex: 1 }}>
              <View style={{ flexDirection: "row", justifyContent: "space-between", alignItems: "center" }}>
                <Text style={{ color: darkTokens.foreground, fontSize: layoutMetrics.bodySize + 1, fontWeight: "600" }}>
                  @{bot.id} · {bot.name}
                </Text>
                {bot.unread && (
                  <View
                    style={{
                      width: 8,
                      height: 8,
                      borderRadius: 8,
                      backgroundColor: darkTokens.unread,
                    }}
                  />
                )}
              </View>
              <Text style={{ color: darkTokens.mutedForeground, fontSize: 12, marginTop: 2 }}>
                {bot.role} · {bot.status}
              </Text>
            </View>
          </Pressable>
        </Link>
      ))}

      <Text
        style={{
          color: darkTokens.faintForeground,
          fontSize: 11,
          fontWeight: "600",
          letterSpacing: 0.6,
          textTransform: "uppercase",
          marginTop: 6,
        }}
      >
        Operations & Readiness
      </Text>

      <Link href="/routines" asChild>
        <Pressable
          style={{
            padding: 15,
            backgroundColor: darkTokens.card,
            borderRadius: 16,
            flexDirection: "row",
            justifyContent: "space-between",
            alignItems: "center",
          }}
        >
          <View>
            <Text style={{ color: darkTokens.foreground, fontSize: 15, fontWeight: "600" }}>
              Routines & Run History
            </Text>
            <Text style={{ color: darkTokens.mutedForeground, fontSize: 12, marginTop: 2 }}>
              IANA timezones, quiet-hours gates & one-tap trigger
            </Text>
          </View>
          <Text style={{ color: darkTokens.link, fontSize: 16 }}>→</Text>
        </Pressable>
      </Link>

      <Link href="/settings" asChild>
        <Pressable
          style={{
            padding: 15,
            backgroundColor: darkTokens.card,
            borderRadius: 16,
            flexDirection: "row",
            justifyContent: "space-between",
            alignItems: "center",
          }}
        >
          <View>
            <Text style={{ color: darkTokens.foreground, fontSize: 15, fontWeight: "600" }}>
              Providers, Workspaces & Doctor
            </Text>
            <Text style={{ color: darkTokens.mutedForeground, fontSize: 12, marginTop: 2 }}>
              5-tier routing precedence, Keychain auth & Doctor health
            </Text>
          </View>
          <Text style={{ color: darkTokens.link, fontSize: 16 }}>→</Text>
        </Pressable>
      </Link>
    </ScrollView>
  );
}
