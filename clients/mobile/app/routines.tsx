import React from "react";
import { View, Text } from "react-native";
import { darkTokens } from "../lib/tokens";
import { mobileV1Endpoint } from "../lib/api";

export default function RoutinesScreen() {
  return (
    <View style={{ flex: 1, backgroundColor: darkTokens.background, padding: 16 }}>
      <Text style={{ color: darkTokens.foreground, fontSize: 18 }}>
        Routines & Run History
      </Text>
      <Text style={{ color: darkTokens.mutedForeground }}>
        Endpoint: {mobileV1Endpoint("", "/routines")}
      </Text>
    </View>
  );
}
