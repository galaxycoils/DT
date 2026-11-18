import React, { useCallback, useState } from "react";
import { Button, ScrollView, Text, TextInput } from "react-native";
import { useFocusEffect } from "expo-router";
import { clearConnection, loadConnection, request, saveConnection, type Connection } from "../lib/api";
import { darkTokens } from "../lib/tokens";

type DoctorStatus = { installation_readiness: string; checks?: Array<{ id: string; summary: string }> };

const inputStyle = { backgroundColor: darkTokens.card, color: darkTokens.foreground, padding: 12, borderRadius: 8 };

export default function SettingsScreen() {
  const [baseUrl, setBaseUrl] = useState("");
  const [token, setToken] = useState("");
  const [workspace, setWorkspace] = useState("");
  const [saved, setSaved] = useState(false);
  const [busy, setBusy] = useState(false);
  const [status, setStatus] = useState("");
  const [doctor, setDoctor] = useState<DoctorStatus | null>(null);

  useFocusEffect(useCallback(() => {
    let active = true;
    void loadConnection().then(async (connection) => {
      if (!connection || !active) return;
      setSaved(true);
      setBaseUrl(connection.baseUrl);
      setWorkspace(connection.workspaceId);
      const report = await request<DoctorStatus>(connection, "/doctor");
      if (active) setDoctor(report);
    }).catch((error) => {
      if (active) setStatus(error.message);
    });
    return () => { active = false; };
  }, []));

  async function connect() {
    setBusy(true);
    setStatus("");
    try {
      const connection: Connection = {
        baseUrl: baseUrl.trim(),
        token: token.trim(),
        workspaceId: workspace.trim() || "default",
      };
      const [session, report] = await Promise.all([
        request<{ active_workspace: string }>(connection, "/workspaces"),
        request<DoctorStatus>(connection, "/doctor"),
      ]);
      await saveConnection({ ...connection, workspaceId: session.active_workspace });
      setToken("");
      setSaved(true);
      setWorkspace(session.active_workspace);
      setDoctor(report);
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
      setStatus("");
    } catch {
      setStatus("Could not remove the saved connection.");
    } finally {
      setBusy(false);
    }
  }

  return (
    <ScrollView style={{ flex: 1, backgroundColor: darkTokens.background }} contentContainerStyle={{ padding: 16, gap: 12 }}>
      <TextInput accessibilityLabel="DT server URL" value={baseUrl} onChangeText={setBaseUrl} autoCapitalize="none" keyboardType="url" placeholder="https://dt.example.com" placeholderTextColor={darkTokens.mutedForeground} style={inputStyle} />
      <TextInput accessibilityLabel="Workspace ID" value={workspace} onChangeText={setWorkspace} autoCapitalize="none" placeholder="Workspace ID" placeholderTextColor={darkTokens.mutedForeground} style={inputStyle} />
      <TextInput accessibilityLabel="Operator-issued access token" value={token} onChangeText={setToken} autoCapitalize="none" secureTextEntry placeholder="Operator-issued access token" placeholderTextColor={darkTokens.mutedForeground} style={inputStyle} />
      <Button title={busy ? "Working…" : "Connect"} disabled={busy || !baseUrl.trim() || !token.trim()} onPress={() => void connect()} />
      {saved && <Button title="Remove connection from this device" disabled={busy} onPress={() => void disconnect()} />}
      {status ? <Text accessibilityLiveRegion="polite" style={{ color: darkTokens.foreground }}>{status}</Text> : null}
      <Text accessibilityRole="header" style={{ color: darkTokens.foreground }}>DT Doctor</Text>
      <Text style={{ color: darkTokens.foreground }}>{doctor?.installation_readiness ?? "Status unavailable"}</Text>
      {doctor?.checks?.map((check) => <Text key={check.id} style={{ color: darkTokens.foreground }}>{check.id} · {check.summary}</Text>)}
    </ScrollView>
  );
}
