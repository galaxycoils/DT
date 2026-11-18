import React, { useCallback, useState } from "react";
import { Button, FlatList, Text, TextInput, View } from "react-native";
import { useFocusEffect, useLocalSearchParams } from "expo-router";
import { connectedRequest, type Message } from "../lib/api";
import { darkTokens } from "../lib/tokens";

export default function ThreadScreen() {
  const { bot = "muse" } = useLocalSearchParams<{ bot?: string }>();
  const endpoint = `/dm/${encodeURIComponent(bot)}/messages`;
  const [messages, setMessages] = useState<Message[]>([]);
  const [draft, setDraft] = useState("");
  const [sending, setSending] = useState(false);
  const [loading, setLoading] = useState(true);
  const [status, setStatus] = useState("");

  const refresh = useCallback(async (signal?: AbortSignal) => {
    setLoading(true);
    try {
      const items = await connectedRequest<Message[]>(endpoint, { signal });
      if (!signal?.aborted) {
        setMessages(items);
        setStatus("");
      }
    } catch (error) {
      if (!signal?.aborted) setStatus(error instanceof Error ? error.message : "Could not load the thread.");
    } finally {
      if (!signal?.aborted) setLoading(false);
    }
  }, [endpoint]);

  useFocusEffect(useCallback(() => {
    const controller = new AbortController();
    void refresh(controller.signal);
    return () => controller.abort();
  }, [refresh]));

  async function send() {
    const content = draft.trim();
    if (!content || sending) return;
    setSending(true);
    try {
      await connectedRequest(endpoint, { method: "POST", body: JSON.stringify({ content }) });
      setDraft("");
      await refresh();
    } catch (error) {
      setStatus(error instanceof Error ? error.message : "Could not send message.");
    } finally {
      setSending(false);
    }
  }

  return (
    <View style={{ flex: 1, backgroundColor: darkTokens.background, padding: 16, gap: 8 }}>
      {status ? <Text accessibilityLiveRegion="polite" style={{ color: darkTokens.foreground }}>{status}</Text> : null}
      <FlatList
        data={messages}
        keyExtractor={(item) => item.id}
        refreshing={loading}
        onRefresh={() => void refresh()}
        ListEmptyComponent={loading || status ? undefined : <Text style={{ color: darkTokens.mutedForeground }}>No messages.</Text>}
        renderItem={({ item }) => <Text style={{ color: darkTokens.foreground, paddingVertical: 8 }}>{item.author} · {item.content}</Text>}
      />
      <TextInput
        accessibilityLabel="Message"
        value={draft}
        editable={!sending}
        onChangeText={setDraft}
        placeholder={`Message @${bot}`}
        placeholderTextColor={darkTokens.mutedForeground}
        style={{ color: darkTokens.foreground, backgroundColor: darkTokens.card, padding: 12, borderRadius: 10 }}
      />
      <Button title={sending ? "Sending…" : "Send"} disabled={sending || !draft.trim()} onPress={() => void send()} />
    </View>
  );
}
