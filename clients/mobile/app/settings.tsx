import React, { useCallback, useState } from "react";
import { Button, ScrollView, Text, TextInput, useColorScheme, View } from "react-native";
import { useFocusEffect } from "expo-router";
import {
  clearConnection,
  loadConnection,
  request,
  saveConnection,
  type Connection,
  type DoctorStatus,
  type ProviderProfileSummary,
} from "../lib/api";
import { darkTokens, lightTokens } from "../lib/tokens";

export default function SettingsScreen() {
  const scheme = useColorScheme();
  const tokens = scheme === "light" ? lightTokens : darkTokens;
  const inputStyle = {
    backgroundColor: tokens.card,
    color: tokens.foreground,
    borderColor: tokens.border,
    borderWidth: 1,
    padding: 12,
    borderRadius: 10,
  };

  const [baseUrl, setBaseUrl] = useState("");
  const [token, setToken] = useState("");
  const [workspace, setWorkspace] = useState("");
  const [saved, setSaved] = useState(false);
  const [busy, setBusy] = useState(false);
  const [status, setStatus] = useState("");
  const [doctor, setDoctor] = useState<DoctorStatus | null>(null);
  const [providers, setProviders] = useState<ProviderProfileSummary[]>([]);

  useFocusEffect(
    useCallback(() => {
      const controller = new AbortController();
      void loadConnection()
        .then(async (connection) => {
          if (!connection || controller.signal.aborted) {
            if (!controller.signal.aborted) {
              setSaved(false);
              setDoctor(null);
              setProviders([]);
            }
            return;
          }
          setSaved(true);
          setBaseUrl(connection.baseUrl);
          setWorkspace(connection.workspaceId);
          const [report, provRes] = await Promise.all([
            request<DoctorStatus>(connection, "/doctor", { signal: controller.signal }),
            request<{ profiles?: ProviderProfileSummary[] }>(connection, "/system/providers", {
              signal: controller.signal,
            }).catch(() => ({ profiles: [] })),
          ]);
          if (!controller.signal.aborted) {
            setDoctor(report);
            setProviders(Array.isArray(provRes?.profiles) ? provRes.profiles : []);
          }
        })
        .catch((error) => {
          if (!controller.signal.aborted) {
            setStatus(error instanceof Error ? error.message : "Could not inspect server.");
          }
        });
      return () => controller.abort();
    }, []),
  );

  async function connect() {
    setBusy(true);
    setStatus("");
    try {
      const connection: Connection = {
        baseUrl: baseUrl.trim(),
        token: token.trim(),
        workspaceId: workspace.trim() || "default",
      };
      const [session, report, provRes] = await Promise.all([
        request<{ active_workspace: string }>(connection, "/workspaces"),
        request<DoctorStatus>(connection, "/doctor"),
        request<{ profiles?: ProviderProfileSummary[] }>(connection, "/system/providers").catch(
          () => ({ profiles: [] }),
        ),
      ]);
      await saveConnection({ ...connection, workspaceId: session.active_workspace });
      setToken("");
      setSaved(true);
      setWorkspace(session.active_workspace);
      setDoctor(report);
      setProviders(Array.isArray(provRes?.profiles) ? provRes.profiles : []);
    } catch (error) {
      setStatus(error instanceof Error ? error.message : "Could not connect.");
    } finally {
      setBusy(false);
    }
  }

  async function disconnect() {
    setBusy(true);
    try {
      await clearConnection();
      setSaved(false);
      setToken("");
      setDoctor(null);
      setProviders([]);
      setStatus("Disconnected from DT server.");
    } catch {
      setStatus("Could not remove the saved connection.");
    } finally {
      setBusy(false);
    }
  }

  return (
    <ScrollView
      style={{ flex: 1, backgroundColor: tokens.background }}
      contentContainerStyle={{ padding: 16, gap: 12 }}
    >
      <Text accessibilityRole="header" style={{ color: tokens.foreground, fontSize: 18, fontWeight: "600" }}>
        Hosted Connection
      </Text>
      <TextInput
        accessibilityLabel="DT server URL"
        value={baseUrl}
        onChangeText={setBaseUrl}
        autoCapitalize="none"
        keyboardType="url"
        placeholder="https://dt.example.com"
        placeholderTextColor={tokens.mutedForeground}
        style={inputStyle}
      />
      <TextInput
        accessibilityLabel="Workspace ID"
        value={workspace}
        onChangeText={setWorkspace}
        autoCapitalize="none"
        placeholder="Workspace ID"
        placeholderTextColor={tokens.mutedForeground}
        style={inputStyle}
      />
      <TextInput
        accessibilityLabel="Operator-issued access token"
        value={token}
        onChangeText={setToken}
        autoCapitalize="none"
        secureTextEntry
        placeholder="Operator-issued access token"
        placeholderTextColor={tokens.mutedForeground}
        style={inputStyle}
      />
      <Button
        title={busy ? "Working…" : "Connect"}
        color={tokens.primary}
        disabled={busy || !baseUrl.trim() || !token.trim()}
        onPress={() => void connect()}
      />
      {saved ? (
        <Button
          title="Remove connection from this device"
          color={tokens.destructive}
          disabled={busy}
          onPress={() => void disconnect()}
        />
      ) : null}
      {status ? (
        <Text accessibilityLiveRegion="polite" style={{ color: tokens.foreground }}>
          {status}
        </Text>
      ) : null}

      <View
        style={{
          backgroundColor: tokens.card,
          borderColor: tokens.border,
          borderWidth: 1,
          borderRadius: 16,
          padding: 14,
          gap: 10,
        }}
      >
        <View style={{ gap: 2 }}>
          <Text accessibilityRole="header" style={{ color: tokens.foreground, fontSize: 15, fontWeight: "600" }}>
            Providers
          </Text>
          <Text style={{ color: tokens.mutedForeground, fontSize: 12 }}>
            Precedence: Routine → Bot → Workspace → Account → Legacy
          </Text>
        </View>
        {providers.length === 0 ? (
          <Text style={{ color: tokens.mutedForeground }}>
            Connect to inspect configured providers.
          </Text>
        ) : (
          providers.map((p) => (
            <View
              key={p.id}
              style={{
                flexDirection: "row",
                justifyContent: "space-between",
                alignItems: "center",
                gap: 8,
              }}
            >
              <View style={{ flex: 1 }}>
                <Text style={{ color: tokens.foreground, fontSize: 14, fontWeight: "600" }}>
                  {p.display_name}
                </Text>
                <Text style={{ color: tokens.mutedForeground, fontSize: 12, marginTop: 2 }}>
                  {p.id}
                  {p.selected_model ? ` · ${p.selected_model}` : ""}
                </Text>
              </View>
              <View
                style={{
                  paddingHorizontal: 10,
                  paddingVertical: 4,
                  borderRadius: 999,
                  backgroundColor: tokens.dialog,
                }}
              >
                <Text
                  style={{
                    color:
                      p.connection_state === "connected"
                        ? tokens.success
                        : tokens.mutedForeground,
                    fontSize: 11,
                    fontWeight: "600",
                  }}
                >
                  {p.connection_state}
                </Text>
              </View>
            </View>
          ))
        )}
      </View>

      <View
        style={{
          backgroundColor: tokens.card,
          borderColor: tokens.border,
          borderWidth: 1,
          borderRadius: 16,
          padding: 14,
          gap: 8,
        }}
      >
        <View style={{ flexDirection: "row", justifyContent: "space-between", alignItems: "center", gap: 8 }}>
          <Text accessibilityRole="header" style={{ color: tokens.foreground, fontSize: 15, fontWeight: "600" }}>
            DT Doctor Readiness (Sanitized)
          </Text>
          <View
            style={{
              paddingHorizontal: 10,
              paddingVertical: 4,
              borderRadius: 999,
              backgroundColor: tokens.dialog,
            }}
          >
            <Text style={{ color: tokens.success, fontSize: 11, fontWeight: "700" }}>
              {doctor?.installation_readiness ?? "Status unavailable"}
            </Text>
          </View>
        </View>
        <Text style={{ color: tokens.mutedForeground, fontSize: 12 }}>
          Local paths & secrets scrubbed before mobile delivery
        </Text>
        {doctor?.checks?.map((check) => (
          <Text key={check.id} style={{ color: tokens.mutedForeground, fontSize: 13 }}>
            {check.id} · {check.summary}
          </Text>
        ))}
      </View>
    </ScrollView>
  );
}
