import React, { useState } from "react";
import { View, Text, Pressable, ScrollView } from "react-native";
import { darkTokens } from "../lib/tokens";
import { mobileV1Endpoint } from "../lib/api";

const PROVIDER_PROFILES = [
  { id: "chatgpt-default", name: "ChatGPT OAuth (PKCE)", state: "connected", model: "gpt-5.4" },
  { id: "meta-muse", name: "Meta Muse API", state: "authenticated", model: "muse-spark-1.3" },
  { id: "xai-grok", name: "xAI Grok API", state: "credentials_saved", model: "grok-4.7" },
  { id: "nous-portal", name: "Nous Portal (Device Grant)", state: "unconfigured", model: "hermes-4" },
];

export default function SettingsScreen() {
  const [doctorReady] = useState("ready");
  const providersEndpoint = mobileV1Endpoint("", "/system/providers");
  const doctorEndpoint = mobileV1Endpoint("", "/doctor");

  return (
    <ScrollView
      style={{ flex: 1, backgroundColor: darkTokens.background }}
      contentContainerStyle={{ padding: 16, gap: 14 }}
    >
      <Text style={{ color: darkTokens.foreground, fontSize: 20, fontWeight: "700" }}>
        Provider Profiles, OAuth & Memory Scopes
      </Text>
      <Text style={{ color: darkTokens.mutedForeground, fontSize: 12 }}>
        Endpoint: {providersEndpoint} · Precedence: Routine → Bot → Workspace → Account → Legacy
      </Text>

      {PROVIDER_PROFILES.map((p) => (
        <View
          key={p.id}
          style={{
            padding: 14,
            backgroundColor: darkTokens.card,
            borderRadius: 14,
            flexDirection: "row",
            justifyContent: "space-between",
            alignItems: "center",
          }}
        >
          <View>
            <Text style={{ color: darkTokens.foreground, fontSize: 14, fontWeight: "600" }}>
              {p.name}
            </Text>
            <Text style={{ color: darkTokens.mutedForeground, fontSize: 12, marginTop: 2 }}>
              {p.id} · {p.model}
            </Text>
          </View>
          <View
            style={{
              paddingHorizontal: 10,
              paddingVertical: 4,
              borderRadius: 999,
              backgroundColor: darkTokens.dialog,
            }}
          >
            <Text
              style={{
                color:
                  p.state === "connected"
                    ? darkTokens.success
                    : darkTokens.mutedForeground,
                fontSize: 11,
                fontWeight: "600",
              }}
            >
              {p.state}
            </Text>
          </View>
        </View>
      ))}

      <View
        style={{
          padding: 15,
          backgroundColor: darkTokens.card,
          borderRadius: 16,
          gap: 6,
          marginTop: 4,
        }}
      >
        <View style={{ flexDirection: "row", justifyContent: "space-between", alignItems: "center" }}>
          <Text style={{ color: darkTokens.foreground, fontSize: 15, fontWeight: "600" }}>
            DT Doctor Readiness (Sanitized)
          </Text>
          <Pressable
            style={{
              paddingHorizontal: 10,
              paddingVertical: 4,
              borderRadius: 999,
              backgroundColor: darkTokens.dialog,
            }}
          >
            <Text style={{ color: darkTokens.success, fontSize: 11, fontWeight: "700" }}>
              {doctorReady}
            </Text>
          </Pressable>
        </View>
        <Text style={{ color: darkTokens.mutedForeground, fontSize: 12 }}>
          Status endpoint: {doctorEndpoint} · Local paths & secrets scrubbed before mobile delivery
        </Text>
      </View>
    </ScrollView>
  );
}
