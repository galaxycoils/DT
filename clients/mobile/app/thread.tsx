import React, { useState } from "react";
import { View, Text, TextInput, Pressable, ScrollView } from "react-native";
import { darkTokens, layoutMetrics } from "../lib/tokens";
import { mobileV1Endpoint } from "../lib/api";

interface ThreadMessage {
  id: string;
  sender: "user" | "muse";
  content: string;
}

export default function ThreadScreen() {
  const [draft, setDraft] = useState("");
  const [approvalPending, setApprovalPending] = useState(true);
  const [messages, setMessages] = useState<ThreadMessage[]>([
    {
      id: "m1",
      sender: "muse",
      content: "Workspace connected. Your thread is non-blocking—inline approvals and background tasks stream while you continue typing.",
    },
  ]);
  const endpoint = mobileV1Endpoint("", "/dm/muse/messages");

  function handleChangeDraft(text: string) {
    setDraft(text.replace(/->/g, "→"));
  }

  function handleSend() {
    const clean = draft.trim();
    if (!clean) return;
    setMessages((prev) => [
      ...prev,
      { id: `u-${Date.now()}`, sender: "user", content: clean },
    ]);
    setDraft("");
  }

  return (
    <View style={{ flex: 1, backgroundColor: darkTokens.background, padding: 16 }}>
      <View
        style={{
          flexDirection: "row",
          justifyContent: "space-between",
          alignItems: "center",
          marginBottom: 12,
        }}
      >
        <Text style={{ color: darkTokens.foreground, fontSize: 16, fontWeight: "600" }}>
          @muse · Non-blocking Thread
        </Text>
        <Text style={{ color: darkTokens.mutedForeground, fontSize: 11 }}>
          {endpoint}
        </Text>
      </View>

      <ScrollView style={{ flex: 1 }} contentContainerStyle={{ gap: 10, paddingBottom: 12 }}>
        {approvalPending && (
          <View
            style={{
              padding: 14,
              backgroundColor: darkTokens.card,
              borderRadius: 14,
              borderWidth: 1,
              borderColor: darkTokens.warning,
              gap: 8,
            }}
          >
            <Text style={{ color: darkTokens.warning, fontWeight: "700", fontSize: 13 }}>
              ⚡ Inline Approval · Export Release Summary
            </Text>
            <Text style={{ color: darkTokens.mutedForeground, fontSize: 12 }}>
              Approve writing release summary to sandbox://outbox/release-notes.md?
            </Text>
            <View style={{ flexDirection: "row", gap: 8 }}>
              <Pressable
                onPress={() => setApprovalPending(false)}
                style={{
                  paddingHorizontal: 14,
                  paddingVertical: 7,
                  backgroundColor: darkTokens.success,
                  borderRadius: 10,
                }}
              >
                <Text style={{ color: "#FFFFFF", fontWeight: "600", fontSize: 12 }}>
                  Approve
                </Text>
              </Pressable>
              <Pressable
                onPress={() => setApprovalPending(false)}
                style={{
                  paddingHorizontal: 14,
                  paddingVertical: 7,
                  backgroundColor: darkTokens.dialog,
                  borderRadius: 10,
                }}
              >
                <Text style={{ color: darkTokens.foreground, fontSize: 12 }}>
                  Deny
                </Text>
              </Pressable>
            </View>
          </View>
        )}

        {messages.map((m) => (
          <View
            key={m.id}
            style={{
              alignSelf: m.sender === "user" ? "flex-end" : "flex-start",
              maxWidth: "85%",
              padding: 12,
              borderRadius: 16,
              backgroundColor:
                m.sender === "user" ? darkTokens.chatUser : darkTokens.card,
            }}
          >
            <Text
              style={{
                color:
                  m.sender === "user"
                    ? darkTokens.chatUserForeground
                    : darkTokens.foreground,
                fontSize: layoutMetrics.bodySize,
                lineHeight: layoutMetrics.bodyLineHeight,
              }}
            >
              {m.content}
            </Text>
          </View>
        ))}
      </ScrollView>

      <View style={{ flexDirection: "row", gap: 6, marginBottom: 8 }}>
        {["/do Check status", "Morning brief -> action items"].map((chip) => (
          <Pressable
            key={chip}
            onPress={() => handleChangeDraft(chip)}
            style={{
              paddingHorizontal: 10,
              paddingVertical: 5,
              borderRadius: 999,
              backgroundColor: darkTokens.card,
            }}
          >
            <Text style={{ color: darkTokens.mutedForeground, fontSize: 11 }}>
              ✦ {chip}
            </Text>
          </Pressable>
        ))}
      </View>

      <View style={{ flexDirection: "row", gap: 8, alignItems: "center" }}>
        <TextInput
          value={draft}
          onChangeText={handleChangeDraft}
          placeholder="Message @muse..."
          placeholderTextColor={darkTokens.mutedForeground}
          style={{
            flex: 1,
            color: darkTokens.foreground,
            backgroundColor: darkTokens.card,
            paddingHorizontal: 14,
            paddingVertical: 11,
            borderRadius: layoutMetrics.composerRadius,
          }}
        />
        <Pressable
          onPress={handleSend}
          style={{
            paddingHorizontal: 16,
            paddingVertical: 11,
            backgroundColor: darkTokens.unread,
            borderRadius: layoutMetrics.composerRadius,
          }}
        >
          <Text style={{ color: darkTokens.chatUserForeground, fontWeight: "600" }}>
            Send
          </Text>
        </Pressable>
      </View>
    </View>
  );
}
