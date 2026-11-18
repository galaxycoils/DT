import React, { useCallback, useState } from "react";
import { Button, ScrollView, Text, View } from "react-native";
import { useFocusEffect } from "expo-router";
import { connectedRequest, type Routine, type RoutineRun } from "../lib/api";
import { darkTokens } from "../lib/tokens";

export default function RoutinesScreen() {
  const [routines, setRoutines] = useState<Routine[]>([]);
  const [selected, setSelected] = useState("");
  const [runs, setRuns] = useState<RoutineRun[]>([]);
  const [status, setStatus] = useState("Loading routines…");

  useFocusEffect(useCallback(() => {
    let active = true;
    void connectedRequest<Routine[]>("/routines").then((items) => {
      if (!active) return;
      setRoutines(items);
      setStatus(items.length ? "" : "No routines.");
    }).catch((error) => {
      if (!active) return;
      setRoutines([]);
      setStatus(error.message);
    });
    return () => { active = false; };
  }, []));

  async function showRuns(routine: Routine) {
    setStatus("Loading runs…");
    setSelected(routine.name);
    setRuns([]);
    try {
      const history = await connectedRequest<RoutineRun[]>(`/routines/${encodeURIComponent(routine.id)}/runs`);
      setRuns(history);
      setStatus(history.length ? "" : "No recorded runs.");
    } catch (error) {
      setStatus(error instanceof Error ? error.message : "Could not load runs.");
    }
  }

  return (
    <ScrollView style={{ flex: 1, backgroundColor: darkTokens.background }} contentContainerStyle={{ padding: 16, gap: 8 }}>
      {routines.map((routine) => <Button key={routine.id} title={`${routine.name} · ${routine.enabled ? "Enabled" : "Disabled"}`} onPress={() => void showRuns(routine)} />)}
      {status ? <Text accessibilityLiveRegion="polite" style={{ color: darkTokens.foreground }}>{status}</Text> : null}
      {selected && <View style={{ gap: 8 }}>
        <Text accessibilityRole="header" style={{ color: darkTokens.foreground }}>{selected}</Text>
        {runs.map((run) => <Text key={run.id} style={{ color: darkTokens.foreground }}>{run.status} · {new Date(run.fired_at).toLocaleString()}</Text>)}
      </View>}
    </ScrollView>
  );
}
