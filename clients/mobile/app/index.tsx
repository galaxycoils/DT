import React, { useCallback, useState } from "react";
import { Button, ScrollView, Text, View } from "react-native";
import { Link, useFocusEffect } from "expo-router";
import { connectedRequest, type Bot } from "../lib/api";
import { darkTokens } from "../lib/tokens";

export default function InboxScreen() {
  const [bots, setBots] = useState<Bot[]>([]);
  const [status, setStatus] = useState("Loading chats…");

  useFocusEffect(useCallback(() => {
    let active = true;
    void connectedRequest<Bot[]>("/bots").then((items) => {
      if (!active) return;
      setBots(items);
      setStatus(items.length ? "" : "No chats.");
    }).catch((error) => {
      if (!active) return;
      setBots([]);
      setStatus(error.message);
    });
    return () => { active = false; };
  }, []));

  return (
    <ScrollView style={{ flex: 1, backgroundColor: darkTokens.background }} contentContainerStyle={{ padding: 16, gap: 12 }}>
      <Link href="/settings" asChild><Button title="Connection & Doctor" /></Link>
      <Link href="/routines" asChild><Button title="Routines & Run History" /></Link>
      {status ? <Text accessibilityLiveRegion="polite" style={{ color: darkTokens.foreground }}>{status}</Text> : null}
      <View style={{ gap: 8 }}>
        {bots.map((bot) => (
          <Link key={bot.id} href={{ pathname: "/thread", params: { bot: bot.id } }} asChild>
            <Button title={bot.display_name || bot.id} />
          </Link>
        ))}
      </View>
    </ScrollView>
  );
}
