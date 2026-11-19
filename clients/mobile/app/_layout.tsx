import React from "react";
import { useColorScheme } from "react-native";
import { Stack } from "expo-router";
import { darkTokens, lightTokens } from "../lib/tokens";

export default function RootLayout() {
  const scheme = useColorScheme();
  const tokens = scheme === "light" ? lightTokens : darkTokens;
  return (
    <Stack
      screenOptions={{
        headerShown: true,
        title: "DT",
        headerStyle: { backgroundColor: tokens.sidebar },
        headerTintColor: tokens.foreground,
        contentStyle: { backgroundColor: tokens.background },
      }}
    />
  );
}
