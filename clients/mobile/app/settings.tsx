import React from "react";
import { View, Text } from "react-native";
import { darkTokens } from "../lib/tokens";
import { mobileV1Endpoint } from "../lib/api";

export default function SettingsScreen() {
  return (
    <View style={{ flex: 1, backgroundColor: darkTokens.background, padding: 16 }}>
      <Text style={{ color: darkTokens.foreground, fontSize: 18 }}>
        Provider Profiles, OAuth & Memory Scopes
      </Text>
      <Text style={{ color: darkTokens.mutedForeground }}>
        Endpoint: {mobileV1Endpoint("", "/system/providers")}
      </Text>
    </View>
  );
}
