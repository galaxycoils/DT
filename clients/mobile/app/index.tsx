import React from "react";
import { View, Text, Pressable } from "react-native";
import { Link } from "expo-router";
import { darkTokens } from "../lib/tokens";

export default function InboxScreen() {
  return (
    <View style={{ flex: 1, backgroundColor: darkTokens.background, padding: 16, gap: 12 }}>
      <Text style={{ color: darkTokens.foreground, fontSize: 20, fontWeight: "600" }}>
        DT
      </Text>
      <Link href="/thread" asChild>
        <Pressable style={{ padding: 14, backgroundColor: darkTokens.card, borderRadius: 12 }}>
          <Text style={{ color: darkTokens.foreground }}>@muse · Direct Thread</Text>
        </Pressable>
      </Link>
      <Link href="/routines" asChild>
        <Pressable style={{ padding: 14, backgroundColor: darkTokens.card, borderRadius: 12 }}>
          <Text style={{ color: darkTokens.foreground }}>Routines & Run History</Text>
        </Pressable>
      </Link>
      <Link href="/settings" asChild>
        <Pressable style={{ padding: 14, backgroundColor: darkTokens.card, borderRadius: 12 }}>
          <Text style={{ color: darkTokens.foreground }}>Providers, Workspaces & Doctor</Text>
        </Pressable>
      </Link>
    </View>
  );
}
