import React, { useCallback, useState } from "react";
import { Button, Pressable, ScrollView, Text, useColorScheme, View } from "react-native";
import { Link, useFocusEffect } from "expo-router";
import {
  connectedRequest,
  listActivity,
  loadConnection,
  type ActivityPost,
  type Bot,
  type Channel,
} from "../lib/api";
import { darkTokens, lightTokens } from "../lib/tokens";

export default function InboxScreen() {
  const scheme = useColorScheme();
  const tokens = scheme === "light" ? lightTokens : darkTokens;
  const [connected, setConnected] = useState<boolean | null>(null);
  const [bots, setBots] = useState<Bot[]>([]);
  const [channels, setChannels] = useState<Channel[]>([]);
  const [posts, setPosts] = useState<ActivityPost[]>([]);
  const [status, setStatus] = useState("Loading workspace…");

  useFocusEffect(
    useCallback(() => {
      const controller = new AbortController();
      void loadConnection()
        .then(async (conn) => {
          if (controller.signal.aborted) return;
          if (!conn) {
            setConnected(false);
            setBots([]);
            setChannels([]);
            setPosts([]);
            setStatus("");
            return;
          }
          setConnected(true);
          setStatus("Loading workspace…");
          const [botList, chanList, actPosts] = await Promise.all([
            connectedRequest<Bot[]>("/bots", { signal: controller.signal }),
            connectedRequest<Channel[]>("/channels", { signal: controller.signal }).catch(() => []),
            listActivity(controller.signal).catch(() => []),
          ]);
          if (controller.signal.aborted) return;
          setBots(Array.isArray(botList) ? botList : []);
          setChannels(Array.isArray(chanList) ? chanList : []);
          setPosts(actPosts);
          setStatus(botList.length ? "" : "No bots configured yet.");
        })
        .catch((error) => {
          if (controller.signal.aborted) return;
          setBots([]);
          setChannels([]);
          setPosts([]);
          setStatus(error instanceof Error ? error.message : "Could not reach DT server.");
        });
      return () => {
        controller.abort();
      };
    }, []),
  );

  if (connected === false) {
    return (
      <ScrollView
        style={{ flex: 1, backgroundColor: tokens.background }}
        contentContainerStyle={{ padding: 20, gap: 16 }}
      >
        <View
          style={{
            backgroundColor: tokens.card,
            borderColor: tokens.border,
            borderWidth: 1,
            borderRadius: 16,
            padding: 18,
            gap: 10,
          }}
        >
          <Text
            accessibilityRole="header"
            style={{ color: tokens.foreground, fontSize: 20, fontWeight: "600" }}
          >
            Connect to DT
          </Text>
          <Text style={{ color: tokens.mutedForeground, fontSize: 15, lineHeight: 22 }}>
            Sign in with your HTTPS DT server origin, workspace ID, and operator access token to
            open chats, approvals, and routines.
          </Text>
          <Link href="/settings" asChild>
            <Button title="Connect to DT Server" color={tokens.primary} />
          </Link>
        </View>
      </ScrollView>
    );
  }

  return (
    <ScrollView
      style={{ flex: 1, backgroundColor: tokens.background }}
      contentContainerStyle={{ padding: 16, gap: 14 }}
    >
      <View style={{ flexDirection: "row", gap: 8, flexWrap: "wrap" }}>
        <Link href="/routines" asChild>
          <Button title="Routines" color={tokens.primary} />
        </Link>
        <Link href="/settings" asChild>
          <Button title="Settings & Doctor" color={tokens.primary} />
        </Link>
      </View>

      {status ? (
        <Text accessibilityLiveRegion="polite" style={{ color: tokens.mutedForeground }}>
          {status}
        </Text>
      ) : null}

      <View style={{ gap: 8 }}>
        <Text style={{ color: tokens.mutedForeground, fontSize: 13, fontWeight: "600" }}>
          Conversations
        </Text>
        {bots.map((bot) => (
          <Link key={bot.id} href={{ pathname: "/thread", params: { bot: bot.id } }} asChild>
            <Pressable
              accessibilityRole="button"
              style={{
                backgroundColor: tokens.card,
                borderColor: tokens.border,
                borderWidth: 1,
                borderRadius: 12,
                padding: 14,
                gap: 2,
              }}
            >
              <Text style={{ color: tokens.foreground, fontSize: 15, fontWeight: "600" }}>
                {bot.display_name || bot.id}
              </Text>
              {bot.role_title ? (
                <Text style={{ color: tokens.mutedForeground, fontSize: 13 }}>
                  {bot.role_title}
                </Text>
              ) : null}
            </Pressable>
          </Link>
        ))}
      </View>

      {channels.length > 0 ? (
        <View style={{ gap: 8 }}>
          <Text style={{ color: tokens.mutedForeground, fontSize: 13, fontWeight: "600" }}>
            Groups
          </Text>
          {channels.map((ch) => (
            <Link
              key={ch.id}
              href={{ pathname: "/thread", params: { channel: ch.id } }}
              asChild
            >
              <Pressable
                accessibilityRole="button"
                style={{
                  backgroundColor: tokens.card,
                  borderColor: tokens.border,
                  borderWidth: 1,
                  borderRadius: 12,
                  padding: 14,
                  gap: 2,
                }}
              >
                <Text style={{ color: tokens.foreground, fontSize: 15, fontWeight: "600" }}>
                  #{ch.name || ch.id}
                </Text>
                {ch.topic ? (
                  <Text style={{ color: tokens.mutedForeground, fontSize: 13 }}>{ch.topic}</Text>
                ) : null}
              </Pressable>
            </Link>
          ))}
        </View>
      ) : null}

      {posts.length > 0 ? (
        <View style={{ gap: 8 }}>
          <Text style={{ color: tokens.mutedForeground, fontSize: 13, fontWeight: "600" }}>
            Recent Activity
          </Text>
          {posts.slice(0, 5).map((post) => (
            <View
              key={post.id}
              style={{
                backgroundColor: tokens.card,
                borderColor: tokens.border,
                borderWidth: 1,
                borderRadius: 12,
                padding: 12,
                gap: 4,
              }}
            >
              <Text style={{ color: tokens.foreground, fontSize: 14, fontWeight: "600" }}>
                {post.title}
              </Text>
              <Text style={{ color: tokens.mutedForeground, fontSize: 13, lineHeight: 19 }}>
                {post.body}
              </Text>
            </View>
          ))}
        </View>
      ) : null}
    </ScrollView>
  );
}
