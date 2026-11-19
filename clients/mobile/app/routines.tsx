import React, { useCallback, useState } from "react";
import { Button, Pressable, ScrollView, Text, useColorScheme, View } from "react-native";
import { useFocusEffect } from "expo-router";
import {
  connectedRequest,
  listRoutineRuns,
  listRoutines,
  type Routine,
  type RoutineRun,
} from "../lib/api";
import { darkTokens, lightTokens } from "../lib/tokens";

export default function RoutinesScreen() {
  const scheme = useColorScheme();
  const tokens = scheme === "light" ? lightTokens : darkTokens;
  const [routines, setRoutines] = useState<Routine[]>([]);
  const [selected, setSelected] = useState<Routine | null>(null);
  const [runs, setRuns] = useState<RoutineRun[]>([]);
  const [status, setStatus] = useState("Loading routines…");

  const reload = useCallback((signal?: AbortSignal) => {
    setStatus("Loading routines…");
    void listRoutines(signal)
      .then((items) => {
        if (signal?.aborted) return;
        setRoutines(items);
        setStatus(items.length ? "" : "No routines configured.");
      })
      .catch((error) => {
        if (signal?.aborted) return;
        setRoutines([]);
        setSelected(null);
        setRuns([]);
        setStatus(error instanceof Error ? error.message : "Could not load routines.");
      });
  }, []);

  useFocusEffect(
    useCallback(() => {
      const controller = new AbortController();
      reload(controller.signal);
      return () => controller.abort();
    }, [reload]),
  );

  async function showRuns(routine: Routine) {
    setStatus("Loading run history…");
    setSelected(routine);
    setRuns([]);
    try {
      const history = await listRoutineRuns(routine.id);
      setRuns(Array.isArray(history) ? history : []);
      setStatus(history.length ? "" : "No recorded runs yet.");
    } catch (error) {
      setStatus(error instanceof Error ? error.message : "Could not load runs.");
    }
  }

  async function triggerTest(routine: Routine) {
    setStatus("Starting test run…");
    try {
      await connectedRequest(`/routines/${encodeURIComponent(routine.id)}/test`, {
        method: "POST",
        body: "{}",
      });
      await showRuns(routine);
    } catch (error) {
      setStatus(error instanceof Error ? error.message : "Could not start test run.");
    }
  }

  return (
    <ScrollView
      style={{ flex: 1, backgroundColor: tokens.background }}
      contentContainerStyle={{ padding: 16, gap: 10 }}
    >
      {routines.map((routine) => {
        const isSelected = selected?.id === routine.id;
        return (
          <View
            key={routine.id}
            style={{
              backgroundColor: tokens.card,
              borderColor: isSelected ? tokens.primary : tokens.border,
              borderWidth: 1,
              borderRadius: 12,
              padding: 12,
              gap: 6,
            }}
          >
            <Pressable accessibilityRole="button" onPress={() => void showRuns(routine)}>
              <Text style={{ color: tokens.foreground, fontSize: 15, fontWeight: "600" }}>
                {routine.name}
              </Text>
              <Text style={{ color: tokens.mutedForeground, fontSize: 13 }}>
                {routine.enabled ? "Enabled" : "Disabled"}
                {routine.next_run_utc ? ` · Next: ${routine.next_run_utc}` : ""}
              </Text>
            </Pressable>
            <View style={{ flexDirection: "row", gap: 8 }}>
              <Button title="Run History" color={tokens.primary} onPress={() => void showRuns(routine)} />
              <Button title="Test Run" color={tokens.primary} onPress={() => void triggerTest(routine)} />
            </View>
          </View>
        );
      })}

      {status ? (
        <Text accessibilityLiveRegion="polite" style={{ color: tokens.mutedForeground }}>
          {status}
        </Text>
      ) : null}

      {selected ? (
        <View
          style={{
            backgroundColor: tokens.card,
            borderColor: tokens.border,
            borderWidth: 1,
            borderRadius: 12,
            padding: 12,
            gap: 6,
          }}
        >
          <Text accessibilityRole="header" style={{ color: tokens.foreground, fontWeight: "600" }}>
            {selected.name} · Run History
          </Text>
          {runs.map((run) => (
            <Text key={run.id} style={{ color: tokens.foreground, fontSize: 13 }}>
              {run.status} · {new Date(run.fired_at).toLocaleString()}
            </Text>
          ))}
        </View>
      ) : null}
    </ScrollView>
  );
}
