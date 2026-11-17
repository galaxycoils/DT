import React, { useState } from "react";
import { View, Text, TextInput, Pressable } from "react-native";
import { darkTokens } from "../lib/tokens";
import { mobileV1Endpoint } from "../lib/api";

export default function ThreadScreen() {
  const [draft, setDraft] = useState("");
  const endpoint = mobileV1Endpoint("", "/dm/muse/messages");
  return (
    <View style={{ flex: 1, backgroundColor: darkTokens.background, padding: 16 }}>
      <Text style={{ color: darkTokens.mutedForeground, marginBottom: 12 }}>
        Non-blocking thread · {endpoint}
      </Text>
      <View style={{ flexDirection: "row", gap: 8, marginTop: "auto" }}>
        <TextInput
          value={draft}
          onChangeText={setDraft}
          placeholder="Message @muse..."
          placeholderTextColor={darkTokens.mutedForeground}
          style={{
            flex: 1,
            color: darkTokens.foreground,
            backgroundColor: darkTokens.card,
            padding: 12,
            borderRadius: 10,
          }}
        />
        <Pressable style={{ padding: 12, backgroundColor: darkTokens.unread, borderRadius: 10 }}>
          <Text style={{ color: darkTokens.chatUserForeground }}>Send</Text>
        </Pressable>
      </View>
    </View>
  );
}
