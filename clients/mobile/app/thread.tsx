import React, { useCallback, useState } from "react";
import { Button, FlatList, Pressable, Text, TextInput, useColorScheme, View } from "react-native";
import { Link, useFocusEffect, useLocalSearchParams } from "expo-router";
import {
  connectedRequest,
  createClientMessageId,
  parseMessageCard,
  postDirectMessage,
  respondApproval,
  type Message,
} from "../lib/api";
import { darkTokens, layoutMetrics, lightTokens } from "../lib/tokens";

interface FailedReplyState {
  clientMsgId: string;
  content: string;
  detail: string;
  providerSetupRequired: boolean;
}

export default function ThreadScreen() {
  const scheme = useColorScheme();
  const tokens = scheme === "light" ? lightTokens : darkTokens;
  const { bot, channel } = useLocalSearchParams<{ bot?: string; channel?: string }>();
  const isChannel = Boolean(channel);
  const targetId = isChannel ? String(channel) : String(bot || "muse");
  const endpoint = isChannel
    ? `/channels/${encodeURIComponent(targetId)}/messages`
    : `/dm/${encodeURIComponent(targetId)}/messages`;

  const [messages, setMessages] = useState<Message[]>([]);
  const [draft, setDraft] = useState("");
  const [sending, setSending] = useState(false);
  const [loading, setLoading] = useState(true);
  const [status, setStatus] = useState("");
  const [failedReply, setFailedReply] = useState<FailedReplyState | null>(null);
  const [pendingClientMsgId, setPendingClientMsgId] = useState<string | null>(null);
  const [cardBusyId, setCardBusyId] = useState<string | null>(null);

  const refresh = useCallback(
    async (signal?: AbortSignal) => {
      setLoading(true);
      try {
        const items = await connectedRequest<Message[]>(endpoint, { signal });
        if (!signal?.aborted) {
          setMessages(Array.isArray(items) ? items : []);
          setStatus("");
        }
      } catch (error) {
        if (!signal?.aborted) {
          setMessages([]);
          setStatus(error instanceof Error ? error.message : "Could not load the thread.");
        }
      } finally {
        if (!signal?.aborted) setLoading(false);
      }
    },
    [endpoint],
  );

  useFocusEffect(
    useCallback(() => {
      const controller = new AbortController();
      setFailedReply(null);
      setPendingClientMsgId(null);
      void refresh(controller.signal);
      return () => controller.abort();
    }, [refresh]),
  );

  function handleChangeDraft(next: string) {
    const transformed = next.replace(/->/g, "→");
    setDraft(transformed);
    if (!transformed.trim()) setPendingClientMsgId(null);
  }

  async function dispatchDirect(content: string, existingId?: string) {
    const clientMsgId = existingId || pendingClientMsgId || createClientMessageId();
    setPendingClientMsgId(clientMsgId);
    setSending(true);
    setStatus("");
    try {
      const outcome = await postDirectMessage(targetId, content, clientMsgId);
      setDraft("");
      setPendingClientMsgId(null);
      if (outcome.reply_error) {
        setFailedReply({
          clientMsgId: outcome.user_message?.id || clientMsgId,
          content,
          detail: outcome.reply_error,
          providerSetupRequired: Boolean(outcome.provider_setup_required),
        });
      } else {
        setFailedReply(null);
      }
      await refresh();
    } catch (error) {
      setStatus(error instanceof Error ? error.message : "Could not send message.");
    } finally {
      setSending(false);
    }
  }

  async function handleSend() {
    const content = draft.trim();
    if (!content || sending) return;
    if (isChannel) {
      setSending(true);
      setStatus("");
      try {
        await connectedRequest(endpoint, {
          method: "POST",
          body: JSON.stringify({ content }),
        });
        setDraft("");
        await refresh();
      } catch (error) {
        setStatus(error instanceof Error ? error.message : "Could not send message.");
      } finally {
        setSending(false);
      }
      return;
    }
    await dispatchDirect(content);
  }

  async function handleApprovalDecision(requestId: string, approved: boolean) {
    setCardBusyId(requestId);
    try {
      const res = await respondApproval(requestId, approved);
      const nextStatus = res.status || (res.resolved ? (approved ? "approved" : "denied") : "expired");
      setMessages((prev) =>
        prev.map((m) => {
          const c = parseMessageCard(m);
          if (c && (c.request_id === requestId || m.request_id === requestId)) {
            return { ...m, card: { ...c, status: nextStatus } };
          }
          return m;
        }),
      );
      await refresh();
    } catch (error) {
      setStatus(error instanceof Error ? error.message : "Could not update approval.");
    } finally {
      setCardBusyId(null);
    }
  }

  return (
    <View style={{ flex: 1, backgroundColor: tokens.background, padding: 16, gap: 10 }}>
      <View
        style={{
          flexDirection: "row",
          justifyContent: "space-between",
          alignItems: "center",
          gap: 8,
        }}
      >
        <Text style={{ color: tokens.foreground, fontSize: 16, fontWeight: "600" }}>
          {isChannel ? `#${targetId}` : `@${targetId}`} · Non-blocking Thread
        </Text>
        <Text style={{ color: tokens.mutedForeground, fontSize: 11 }}>
          {endpoint}
        </Text>
      </View>

      {status ? (
        <Text accessibilityLiveRegion="polite" style={{ color: tokens.destructive }}>
          {status}
        </Text>
      ) : null}

      <FlatList
        data={messages}
        keyExtractor={(item) => item.id}
        refreshing={loading}
        onRefresh={() => void refresh()}
        contentContainerStyle={{ gap: 10, paddingBottom: 8 }}
        ListEmptyComponent={
          loading || status ? undefined : (
            <Text style={{ color: tokens.mutedForeground }}>
              No messages yet in {isChannel ? `#${targetId}` : `@${targetId}`}.
            </Text>
          )
        }
        renderItem={({ item }) => {
          const card = parseMessageCard(item);
          const cardKind = card?.kind || card?.type || "";
          const isApproval = ["approval", "terminal_approval", "ask_first", "order_approval"].includes(
            cardKind,
          );
          const cardStatus = card?.status || "pending";
          const reqId = String(card?.request_id || item.request_id || item.id);
          const isUser = item.role === "user" || item.author === "You";
          return (
            <View
              style={{
                backgroundColor: isUser ? tokens.chatUser : tokens.card,
                borderColor: tokens.border,
                borderWidth: 1,
                borderRadius: 16,
                padding: 12,
                gap: 6,
                alignSelf: isUser ? "flex-end" : "stretch",
                maxWidth: "94%",
              }}
            >
              <Text
                style={{
                  color: isUser ? tokens.chatUserForeground : tokens.mutedForeground,
                  fontSize: 12,
                  fontWeight: "600",
                }}
              >
                {item.author || targetId}
              </Text>
              {item.content ? (
                <Text
                  style={{
                    color: isUser ? tokens.chatUserForeground : tokens.foreground,
                    fontSize: layoutMetrics.bodySize,
                    lineHeight: layoutMetrics.bodyLineHeight,
                  }}
                >
                  {item.content}
                </Text>
              ) : null}
              {isApproval && card ? (
                <View
                  style={{
                    marginTop: 6,
                    padding: 10,
                    borderRadius: 12,
                    borderWidth: 1,
                    borderColor: cardStatus === "pending" ? tokens.warning : tokens.border,
                    backgroundColor: tokens.background,
                    gap: 6,
                  }}
                >
                  <Text
                    style={{
                      color: cardStatus === "pending" ? tokens.warning : tokens.foreground,
                      fontWeight: "700",
                      fontSize: 13,
                    }}
                  >
                    ⚡ Inline Approval · {card.title || "Action Approval Required"} · [{cardStatus}]
                  </Text>
                  {card.details || card.summary ? (
                    <Text style={{ color: tokens.mutedForeground, fontSize: 13 }}>
                      {String(card.details || card.summary)}
                    </Text>
                  ) : null}
                  {cardStatus === "pending" ? (
                    <View style={{ flexDirection: "row", gap: 8 }}>
                      <Button
                        title="Approve"
                        color={tokens.success}
                        disabled={cardBusyId === reqId}
                        onPress={() => void handleApprovalDecision(reqId, true)}
                      />
                      <Button
                        title="Deny"
                        color={tokens.destructive}
                        disabled={cardBusyId === reqId}
                        onPress={() => void handleApprovalDecision(reqId, false)}
                      />
                    </View>
                  ) : null}
                </View>
              ) : null}
            </View>
          );
        }}
      />

      {failedReply ? (
        <View
          accessibilityLiveRegion="polite"
          style={{
            backgroundColor: tokens.card,
            borderColor: tokens.warning,
            borderWidth: 1,
            borderRadius: 12,
            padding: 12,
            gap: 8,
          }}
        >
          <Text style={{ color: tokens.warning, fontWeight: "600" }}>
            Message saved, reply failed
          </Text>
          <Text style={{ color: tokens.mutedForeground, fontSize: 13 }}>{failedReply.detail}</Text>
          <View style={{ flexDirection: "row", gap: 8, flexWrap: "wrap" }}>
            <Button
              title="Retry reply"
              color={tokens.primary}
              disabled={sending}
              onPress={() => void dispatchDirect(failedReply.content, failedReply.clientMsgId)}
            />
            {failedReply.providerSetupRequired ? (
              <Link href="/settings" asChild>
                <Button title="Open Provider Setup" color={tokens.primary} />
              </Link>
            ) : null}
          </View>
        </View>
      ) : null}

      <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 6 }}>
        {["/do Check status", "Morning brief -> action items"].map((chip) => (
          <Pressable
            key={chip}
            disabled={sending}
            onPress={() => handleChangeDraft(chip)}
            style={{
              paddingHorizontal: 10,
              paddingVertical: 5,
              borderRadius: 999,
              backgroundColor: tokens.card,
              borderColor: tokens.border,
              borderWidth: 1,
            }}
          >
            <Text style={{ color: tokens.mutedForeground, fontSize: 11 }}>
              ✦ {chip}
            </Text>
          </Pressable>
        ))}
      </View>

      <View style={{ flexDirection: "row", gap: 8, alignItems: "center" }}>
        <TextInput
          accessibilityLabel="Message"
          value={draft}
          editable={!sending}
          onChangeText={handleChangeDraft}
          placeholder={isChannel ? `Message #${targetId}` : `Message @${targetId}`}
          placeholderTextColor={tokens.mutedForeground}
          style={{
            flex: 1,
            color: tokens.foreground,
            backgroundColor: tokens.card,
            borderColor: tokens.border,
            borderWidth: 1,
            paddingHorizontal: 14,
            paddingVertical: 10,
            borderRadius: layoutMetrics.composerRadius,
          }}
        />
        <Pressable
          accessibilityRole="button"
          disabled={sending || !draft.trim()}
          onPress={() => void handleSend()}
          style={{
            paddingHorizontal: 16,
            paddingVertical: 11,
            backgroundColor: tokens.primary,
            borderRadius: layoutMetrics.composerRadius,
            opacity: sending || !draft.trim() ? 0.5 : 1,
          }}
        >
          <Text style={{ color: tokens.primaryForeground, fontWeight: "600" }}>
            {sending ? "Sending…" : "Send"}
          </Text>
        </Pressable>
      </View>
    </View>
  );
}
