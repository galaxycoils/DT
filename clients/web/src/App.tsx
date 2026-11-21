import React, { useEffect, useMemo, useRef, useState } from "react";
import { DtV1Client, applySmartArrowEdit } from "./api";

interface PendingApproval {
  request_id: string;
  title: string;
  details: string;
  tier?: string;
}

interface BotSummary {
  id: string;
  display_name?: string;
  role_title?: string;
  avatar_color?: string;
  avatar_shape?: string;
  status_phrase?: string;
  needs_attention?: boolean;
}

interface ChannelSummary {
  id: string;
  topic?: string;
  participants?: string[];
}

interface ChatMessageItem {
  id: string;
  sender?: string;
  author?: string;
  content: string;
  created_at?: string;
}

interface ProviderSummary {
  id: string;
  display_name: string;
  preset?: string;
  connection_state: string;
  selected_model?: string;
}

interface RoutineSummary {
  id: string;
  name?: string;
  bot_id?: string;
  instruction?: string;
  timezone?: string;
  hour?: number;
  minute?: number;
  enabled?: boolean;
  next_run_utc?: string;
}

interface SpaceSummary {
  slug: string;
  title?: string;
  published?: boolean;
}

export type ViewTab = "chats" | "activity" | "spaces" | "providers" | "doctor" | "settings";

export const DRAFT_STORAGE_KEY = "dt-web-drafts-v1";

const DEFAULT_BOTS: BotSummary[] = [
  { id: "muse", display_name: "Muse", role_title: "Primary Personal Agent", avatar_color: "#4D9BFF", avatar_shape: "spark", status_phrase: "Ready for tasks" },
  { id: "researcher", display_name: "Researcher", role_title: "Wide Research & Citations", avatar_color: "#00BA69", avatar_shape: "hex", status_phrase: "Provenance indexed" },
  { id: "coder", display_name: "Coder", role_title: "Isolated Worktree & Patch", avatar_color: "#E28600", avatar_shape: "rounded_square", status_phrase: "VM sandbox idle" },
  { id: "guardian", display_name: "Guardian", role_title: "Safety & Discretion Audit", avatar_color: "#9B6BFF", avatar_shape: "diamond", status_phrase: "Zero policy alerts" },
];

const DEFAULT_CHANNELS: ChannelSummary[] = [
  { id: "general", topic: "Team coordination & multi-bot @mentions", participants: ["muse", "researcher", "coder"] },
  { id: "research", topic: "Source synthesis & citation verification", participants: ["researcher", "muse"] },
  { id: "ops", topic: "Routines, diagnostics & patch review", participants: ["coder", "guardian"] },
];

const STARTER_PROMPTS: Record<string, Array<{ label: string; prompt: string }>> = {
  muse: [
    { label: "Morning brief", prompt: "Summarize today's priorities, active goals, and any due follow-ups -> action items." },
    { label: "Desktop task", prompt: "/do Inspect the current workspace and draft a clean status summary." },
    { label: "Build a Page", prompt: "Create an interactive release readiness dashboard in a static Space." },
  ],
  researcher: [
    { label: "Verify sources", prompt: "Cross-check our recent research notes and attach provenance citations." },
    { label: "Compare options", prompt: "Build a structured comparison table with trade-offs and recommendations." },
  ],
  coder: [
    { label: "Review patch", prompt: "Inspect the latest worktree diff and verify all safety invariants pass." },
    { label: "Test coverage", prompt: "Audit our test suite against the v1.0.0 coverage inventory." },
  ],
  guardian: [
    { label: "Discretion audit", prompt: "Check outbound bridge discretion rules and verify no private facts can leak." },
    { label: "Budget check", prompt: "Inspect current daily and release-ledger inference exposure." },
  ],
};

export function loadTargetDraft(storage: Pick<Storage, "getItem"> | null | undefined, targetKey: string): string {
  if (!storage) return "";
  try {
    const raw = storage.getItem(DRAFT_STORAGE_KEY);
    if (!raw) return "";
    const parsed = JSON.parse(raw) as Record<string, string>;
    return typeof parsed[targetKey] === "string" ? parsed[targetKey] : "";
  } catch {
    return "";
  }
}

export function saveTargetDraft(
  storage: Pick<Storage, "getItem" | "setItem"> | null | undefined,
  targetKey: string,
  value: string,
): void {
  if (!storage || !targetKey) return;
  try {
    const raw = storage.getItem(DRAFT_STORAGE_KEY);
    const map = raw ? (JSON.parse(raw) as Record<string, string>) : {};
    if (value) {
      map[targetKey] = value;
    } else {
      delete map[targetKey];
    }
    storage.setItem(DRAFT_STORAGE_KEY, JSON.stringify(map));
  } catch {
    // Ignore storage quota errors in restricted environments
  }
}

function getBrowserStorage(): Storage | null {
  try {
    return typeof window !== "undefined" ? window.localStorage : null;
  } catch {
    return null;
  }
}

function BotAvatar({
  bot,
  size = 34,
}: {
  bot: { id: string; avatar_color?: string; avatar_shape?: string };
  size?: number;
}) {
  const shapes: Record<string, string> = {
    muse: "spark",
    researcher: "hex",
    coder: "rounded_square",
    guardian: "diamond",
  };
  const colors: Record<string, string> = {
    muse: "#4D9BFF",
    researcher: "#00BA69",
    coder: "#E28600",
    guardian: "#9B6BFF",
  };
  const shape = bot.avatar_shape || shapes[bot.id] || "circle";
  const bg = bot.avatar_color || colors[bot.id] || "#4D9BFF";
  return (
    <span
      className={`dt-avatar ${shape}`}
      style={{ width: size, height: size, background: bg }}
      aria-hidden="true"
    >
      <span className="dt-eyes" style={{ transform: size < 28 ? "scale(0.75)" : undefined }} />
    </span>
  );
}

export function App({ client }: { client: DtV1Client }) {
  const [view, setView] = useState<ViewTab>("chats");
  const [theme, setTheme] = useState<"dark" | "light">("dark");
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false);
  const [sidebarWidth, setSidebarWidth] = useState(316);
  const [mobileMenuOpen, setMobileMenuOpen] = useState(false);
  const [commandOpen, setCommandOpen] = useState(false);
  const [commandQuery, setCommandQuery] = useState("");

  const [bots, setBots] = useState<BotSummary[]>(DEFAULT_BOTS);
  const [channels, setChannels] = useState<ChannelSummary[]>(DEFAULT_CHANNELS);
  const [targetKind, setTargetKind] = useState<"dm" | "channel">("dm");
  const [selectedTarget, setSelectedTarget] = useState("muse");
  const targetKey = `${targetKind}:${selectedTarget}`;

  const [draft, setDraft] = useState(() =>
    loadTargetDraft(getBrowserStorage(), "dm:muse") || loadTargetDraft(getBrowserStorage(), "muse"),
  );
  const [messages, setMessages] = useState<ChatMessageItem[]>([]);
  const [approvals, setApprovals] = useState<PendingApproval[]>([]);
  const [providers, setProviders] = useState<ProviderSummary[]>([]);
  const [routines, setRoutines] = useState<RoutineSummary[]>([]);
  const [routineTriggerResult, setRoutineTriggerResult] = useState<Record<string, string>>({});
  const [spaces, setSpaces] = useState<SpaceSummary[]>([]);
  const [splitSpaceSlug, setSplitSpaceSlug] = useState<string | null>(null);
  const [splitSpaceUrl, setSplitSpaceUrl] = useState<string | null>(null);
  const [newSpaceSlug, setNewSpaceSlug] = useState("");
  const [newSpaceTitle, setNewSpaceTitle] = useState("");
  const [doctor, setDoctor] = useState<Record<string, unknown> | null>(null);
  const [showRawDoctor, setShowRawDoctor] = useState(false);
  const [probeResults, setProbeResults] = useState<Record<string, string>>({});
  const [workspaceId, setWorkspaceId] = useState(client.getWorkspaceId());
  const [enrollmentInput, setEnrollmentInput] = useState("");
  const [statusError, setStatusError] = useState<string | null>(null);
  const [statusNotice, setStatusNotice] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const resizingRef = useRef(false);

  useEffect(() => {
    document.documentElement.setAttribute("data-theme", theme);
  }, [theme]);

  // Global keyboard shortcuts: Cmd/Ctrl+K (Command Palette), Cmd/Ctrl+B (Sidebar toggle), Escape
  useEffect(() => {
    function onKeyDown(e: KeyboardEvent) {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setCommandOpen((open) => !open);
        setCommandQuery("");
      } else if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "b") {
        e.preventDefault();
        setSidebarCollapsed((c) => !c);
      } else if (e.key === "Escape") {
        if (commandOpen) setCommandOpen(false);
        else if (mobileMenuOpen) setMobileMenuOpen(false);
        else if (splitSpaceSlug) {
          setSplitSpaceSlug(null);
          setSplitSpaceUrl(null);
        }
      }
    }
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [commandOpen, mobileMenuOpen, splitSpaceSlug]);

  // Sidebar drag-to-resize (240px - 480px)
  useEffect(() => {
    function onMouseMove(e: MouseEvent) {
      if (!resizingRef.current) return;
      const clamped = Math.max(240, Math.min(480, e.clientX));
      setSidebarWidth(clamped);
    }
    function onMouseUp() {
      resizingRef.current = false;
    }
    window.addEventListener("mousemove", onMouseMove);
    window.addEventListener("mouseup", onMouseUp);
    return () => {
      window.removeEventListener("mousemove", onMouseMove);
      window.removeEventListener("mouseup", onMouseUp);
    };
  }, []);

  function selectConversation(kind: "dm" | "channel", id: string) {
    const nextKey = `${kind}:${id}`;
    const storage = getBrowserStorage();
    const restored =
      loadTargetDraft(storage, nextKey) ||
      (kind === "dm" ? loadTargetDraft(storage, id) : "");
    setTargetKind(kind);
    setSelectedTarget(id);
    setDraft(restored);
    setView("chats");
    setMobileMenuOpen(false);
  }

  function updateDraft(nextValue: string) {
    const transformed = applySmartArrowEdit(draft, nextValue);
    setDraft(transformed);
    const storage = getBrowserStorage();
    saveTargetDraft(storage, targetKey, transformed);
    if (targetKind === "dm") {
      saveTargetDraft(storage, selectedTarget, transformed);
    }
  }

  async function loadThreadMessages(kind: "dm" | "channel", id: string) {
    try {
      if (kind === "dm") {
        const res = await client.getDirectMessages(id);
        const list = Array.isArray(res)
          ? (res as unknown as ChatMessageItem[])
          : Array.isArray((res as Record<string, unknown>).messages)
            ? ((res as Record<string, unknown>).messages as ChatMessageItem[])
            : [];
        setMessages(list);
      } else {
        const list = await client.getChannelMessages(id);
        setMessages(Array.isArray(list) ? (list as unknown as ChatMessageItem[]) : []);
      }
    } catch {
      // Keep current messages if offline
    }
  }

  async function refreshAll() {
    setLoading(true);
    setStatusError(null);
    try {
      const [snap, prov, doc] = await Promise.all([
        client.bootstrap(),
        client.getProviders(),
        client.getDoctorStatus().catch(() => null),
      ]);
      if (Array.isArray(snap.bots) && snap.bots.length > 0) {
        setBots(snap.bots as BotSummary[]);
      }
      if (Array.isArray(snap.channels) && snap.channels.length > 0) {
        setChannels(snap.channels as ChannelSummary[]);
      }
      if (Array.isArray(snap.dm_messages)) {
        setMessages(snap.dm_messages as ChatMessageItem[]);
      }
      if (Array.isArray(snap.pending_approvals)) {
        setApprovals(snap.pending_approvals as PendingApproval[]);
      }
      if (Array.isArray(snap.routines)) {
        setRoutines(snap.routines as RoutineSummary[]);
      } else {
        const rList = await client.listRoutines().catch(() => []);
        if (Array.isArray(rList)) setRoutines(rList as unknown as RoutineSummary[]);
      }
      if (Array.isArray(prov.profiles)) {
        setProviders(prov.profiles as ProviderSummary[]);
      }
      if (doc) setDoctor(doc);
      const sp = await client.listSpaces().catch(() => []);
      if (Array.isArray(sp)) setSpaces(sp as SpaceSummary[]);
    } catch (err) {
      setStatusError(err instanceof Error ? err.message : "Failed to load DT workspace");
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    void refreshAll();
  }, [client, workspaceId]);

  useEffect(() => {
    void loadThreadMessages(targetKind, selectedTarget);
  }, [client, targetKind, selectedTarget]);

  async function send(overridePrompt?: string) {
    const text = (overridePrompt ?? draft).trim();
    if (!text) return;
    setStatusError(null);
    try {
      if (targetKind === "dm") {
        await client.postDirectMessage(selectedTarget, text);
      } else {
        await client.postChannelMessage(selectedTarget, text);
      }
      if (overridePrompt === undefined) {
        updateDraft("");
      }
      await loadThreadMessages(targetKind, selectedTarget);
    } catch (err) {
      setStatusError(err instanceof Error ? err.message : "Failed to send message; draft preserved.");
    }
  }

  async function handleApproval(requestId: string, approved: boolean) {
    try {
      await client.respondApproval({ request_id: requestId, approved });
      setApprovals((prev) => prev.filter((a) => a.request_id !== requestId));
      setStatusNotice(approved ? "Approved request." : "Denied request.");
    } catch (err) {
      setStatusError(err instanceof Error ? err.message : "Could not submit approval response");
    }
  }

  async function handleTriggerRoutine(routineId: string) {
    const deliveryId = `web-${routineId}-${Date.now()}`;
    try {
      const res = await client.triggerRoutine(routineId, deliveryId);
      const status = typeof res.status === "string" ? res.status : "queued";
      setRoutineTriggerResult((prev) => ({ ...prev, [routineId]: status }));
    } catch (err) {
      setRoutineTriggerResult((prev) => ({
        ...prev,
        [routineId]: err instanceof Error ? err.message : "error",
      }));
    }
  }

  async function handleOpenSplitSpace(slug: string) {
    setSplitSpaceSlug(slug);
    try {
      const res = await client.getSpaceUrl(slug);
      setSplitSpaceUrl(res.url);
    } catch {
      setSplitSpaceUrl(`/spaces/v2/${encodeURIComponent(slug)}/`);
    }
  }

  async function handleCreateSpace(e: React.FormEvent) {
    e.preventDefault();
    const slug = newSpaceSlug.trim().toLowerCase().replace(/[^a-z0-9-_]/g, "-");
    if (!slug) return;
    const title = newSpaceTitle.trim() || slug;
    try {
      await client.createSpace(slug, title);
      setNewSpaceSlug("");
      setNewSpaceTitle("");
      const sp = await client.listSpaces().catch(() => []);
      if (Array.isArray(sp)) setSpaces(sp as SpaceSummary[]);
      await handleOpenSplitSpace(slug);
    } catch (err) {
      setStatusError(err instanceof Error ? err.message : "Failed to create Page");
    }
  }

  async function handleProbeConnector(connectorId: string) {
    setProbeResults((prev) => ({ ...prev, [connectorId]: "probing…" }));
    try {
      const res = await client.probeIntegration(connectorId);
      const status = typeof res.status === "string" ? res.status : "unconfigured";
      setProbeResults((prev) => ({ ...prev, [connectorId]: status }));
    } catch {
      setProbeResults((prev) => ({ ...prev, [connectorId]: "setup_required" }));
    }
  }

  async function handleEnrollSignIn(e: React.FormEvent) {
    e.preventDefault();
    if (!enrollmentInput.trim()) return;
    setStatusError(null);
    try {
      const sess = await client.redeemEnrollment(enrollmentInput.trim());
      setEnrollmentInput("");
      setWorkspaceId(sess.ws_id);
      setStatusNotice(`Connected to workspace ${sess.ws_id} via HttpOnly session.`);
      await refreshAll();
    } catch (err) {
      setStatusError(err instanceof Error ? err.message : "Enrollment token rejected");
    }
  }

  const activeBot = useMemo(
    () => bots.find((b) => b.id === selectedTarget) || DEFAULT_BOTS[0],
    [bots, selectedTarget],
  );
  const activeChannel = useMemo(
    () => channels.find((c) => c.id === selectedTarget) || DEFAULT_CHANNELS[0],
    [channels, selectedTarget],
  );

  const verifiedProvider = useMemo(
    () =>
      providers.find((p) => p.connection_state === "connected") ||
      providers.find((p) => p.connection_state === "authenticated") ||
      providers[0],
    [providers],
  );

  const commandItems = useMemo(() => {
    const items: Array<{ id: string; label: string; category: string; run: () => void }> = [];
    for (const b of bots) {
      items.push({
        id: `bot-${b.id}`,
        label: `@${b.id} · ${b.display_name || b.id} (${b.role_title || "Coworker"})`,
        category: "Coworker Bots",
        run: () => selectConversation("dm", b.id),
      });
    }
    for (const ch of channels) {
      items.push({
        id: `ch-${ch.id}`,
        label: `#${ch.id} · ${ch.topic || "Multi-bot channel"}`,
        category: "Channels",
        run: () => selectConversation("channel", ch.id),
      });
    }
    const navs: Array<{ tab: ViewTab; title: string }> = [
      { tab: "chats", title: "Go to Conversation Thread" },
      { tab: "activity", title: "Go to Activity & Scheduled Routines" },
      { tab: "spaces", title: "Go to Pages & Static Spaces" },
      { tab: "providers", title: "Go to Providers, OAuth & Connectors" },
      { tab: "doctor", title: "Go to DT Doctor Readiness Scorecard" },
      { tab: "settings", title: "Go to Workspace & Session Settings" },
    ];
    for (const n of navs) {
      items.push({
        id: `nav-${n.tab}`,
        label: n.title,
        category: "Navigation",
        run: () => setView(n.tab),
      });
    }
    items.push({
      id: "act-theme",
      label: `Switch to ${theme === "dark" ? "Light" : "Dark"} Theme`,
      category: "Appearance",
      run: () => setTheme((t) => (t === "dark" ? "light" : "dark")),
    });
    const q = commandQuery.trim().toLowerCase();
    return q ? items.filter((i) => i.label.toLowerCase().includes(q) || i.category.toLowerCase().includes(q)) : items;
  }, [bots, channels, commandQuery, theme]);

  const doctorChecks = useMemo(() => {
    if (!doctor || !Array.isArray(doctor.checks)) return [];
    return doctor.checks as Array<{ id: string; category: string; outcome: string; summary: string }>;
  }, [doctor]);

  const doctorCapabilities = useMemo(() => {
    if (!doctor || !Array.isArray(doctor.capability_readiness)) return [];
    return doctor.capability_readiness as Array<{ id: string; name: string; readiness: string; summary: string }>;
  }, [doctor]);

  return (
    <div className="dt-shell">
      <div
        className="dt-mobile-backdrop"
        data-mobile-open={mobileMenuOpen ? "true" : "false"}
        onClick={() => setMobileMenuOpen(false)}
      />

      <aside
        className="dt-sidebar"
        data-mobile-open={mobileMenuOpen ? "true" : "false"}
        style={{
          width: sidebarCollapsed ? 72 : sidebarWidth,
          minWidth: sidebarCollapsed ? 72 : 240,
          maxWidth: 480,
        }}
      >
        <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", padding: "0 4px" }}>
          {!sidebarCollapsed ? (
            <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
              <span className="dt-working-dot" title="Runtime active" />
              <strong style={{ fontSize: 15, letterSpacing: "-0.01em" }}>DT · {workspaceId}</strong>
            </div>
          ) : (
            <strong style={{ fontSize: 14, margin: "0 auto" }}>DT</strong>
          )}
          <button
            type="button"
            aria-label="Toggle sidebar"
            title="Toggle sidebar (Ctrl+B)"
            onClick={() => setSidebarCollapsed((c) => !c)}
            style={{
              width: 30,
              height: 30,
              borderRadius: 8,
              border: "1px solid var(--border)",
              background: "var(--card)",
              display: "inline-grid",
              placeItems: "center",
            }}
          >
            {sidebarCollapsed ? "→" : "←"}
          </button>
        </div>

        <nav aria-label="Sections" style={{ display: "grid", gridTemplateColumns: sidebarCollapsed ? "1fr" : "repeat(3, 1fr)", gap: 4 }}>
          {(
            [
              { id: "chats", label: "Chats", icon: "💬" },
              { id: "activity", label: "Routines", icon: "⚡" },
              { id: "spaces", label: "Pages", icon: "◧" },
              { id: "providers", label: "Models", icon: "◈" },
              { id: "doctor", label: "Doctor", icon: "✚" },
              { id: "settings", label: "Settings", icon: "⚙" },
            ] as Array<{ id: ViewTab; label: string; icon: string }>
          ).map((tab) => {
            const active = view === tab.id;
            return (
              <button
                key={tab.id}
                type="button"
                onClick={() => {
                  setView(tab.id);
                  setMobileMenuOpen(false);
                }}
                style={{
                  padding: sidebarCollapsed ? "8px 4px" : "7px 8px",
                  borderRadius: 10,
                  border: active ? "1px solid var(--input)" : "1px solid transparent",
                  background: active ? "var(--card)" : "transparent",
                  color: active ? "var(--foreground)" : "var(--muted-foreground)",
                  fontSize: 12,
                  fontWeight: active ? 600 : 400,
                  textAlign: "center",
                }}
                title={tab.label}
              >
                {sidebarCollapsed ? tab.icon : tab.label}
              </button>
            );
          })}
        </nav>

        <div style={{ flex: 1, overflowY: "auto", display: "flex", flexDirection: "column", gap: 4, paddingTop: 4 }}>
          {!sidebarCollapsed && (
            <small style={{ padding: "6px 10px 2px", fontSize: 11, color: "var(--faint-foreground)", textTransform: "uppercase", letterSpacing: "0.05em" }}>
              Coworkers
            </small>
          )}
          {bots.map((b) => {
            const isSelected = view === "chats" && targetKind === "dm" && selectedTarget === b.id;
            return (
              <button
                key={b.id}
                type="button"
                className={`dt-chat-item ${isSelected ? "active" : ""}`}
                onClick={() => selectConversation("dm", b.id)}
                title={`${b.display_name || b.id} (${b.role_title || "Agent"})`}
                style={{ justifyContent: sidebarCollapsed ? "center" : "flex-start" }}
              >
                <BotAvatar bot={b} size={34} />
                {!sidebarCollapsed && (
                  <div style={{ minWidth: 0, flex: 1 }}>
                    <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", gap: 6 }}>
                      <strong style={{ fontSize: 13.5, fontWeight: 550, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                        {b.display_name || b.id}
                      </strong>
                      {b.needs_attention && (
                        <span style={{ background: "var(--unread)", color: "#fff", borderRadius: 99, padding: "0 6px", fontSize: 10 }}>
                          !
                        </span>
                      )}
                    </div>
                    <small style={{ display: "block", fontSize: 12, color: "var(--muted-foreground)", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                      {b.status_phrase || b.role_title || `@${b.id}`}
                    </small>
                  </div>
                )}
              </button>
            );
          })}

          {!sidebarCollapsed && (
            <small style={{ padding: "12px 10px 2px", fontSize: 11, color: "var(--faint-foreground)", textTransform: "uppercase", letterSpacing: "0.05em" }}>
              Multi-Bot Channels
            </small>
          )}
          {channels.map((ch) => {
            const isSelected = view === "chats" && targetKind === "channel" && selectedTarget === ch.id;
            return (
              <button
                key={ch.id}
                type="button"
                className={`dt-chat-item ${isSelected ? "active" : ""}`}
                onClick={() => selectConversation("channel", ch.id)}
                title={`#${ch.id}`}
                style={{ justifyContent: sidebarCollapsed ? "center" : "flex-start", padding: "7px 11px" }}
              >
                <span
                  style={{
                    width: 28,
                    height: 28,
                    borderRadius: 8,
                    background: "var(--card)",
                    border: "1px solid var(--border)",
                    display: "inline-grid",
                    placeItems: "center",
                    fontSize: 13,
                    fontWeight: 600,
                    color: "var(--muted-foreground)",
                  }}
                >
                  #
                </span>
                {!sidebarCollapsed && (
                  <div style={{ minWidth: 0, flex: 1 }}>
                    <strong style={{ fontSize: 13, fontWeight: 520, display: "block" }}>{ch.id}</strong>
                    {ch.topic && (
                      <small style={{ display: "block", fontSize: 11, color: "var(--faint-foreground)", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                        {ch.topic}
                      </small>
                    )}
                  </div>
                )}
              </button>
            );
          })}
        </div>

        {!sidebarCollapsed && (
          <footer style={{ borderTop: "1px solid var(--sidebar-border)", paddingTop: 10, display: "flex", gap: 6 }}>
            <button
              type="button"
              onClick={() => setCommandOpen(true)}
              style={{
                flex: 1,
                padding: "7px 10px",
                borderRadius: 10,
                border: "1px solid var(--border)",
                background: "var(--card)",
                color: "var(--muted-foreground)",
                fontSize: 12,
                display: "flex",
                justifyContent: "space-between",
                alignItems: "center",
              }}
            >
              <span>Quick Jump…</span>
              <kbd style={{ fontSize: 10, opacity: 0.75 }}>⌘K</kbd>
            </button>
            <button
              type="button"
              onClick={() => setTheme((t) => (t === "dark" ? "light" : "dark"))}
              aria-label="Toggle theme"
              style={{
                padding: "7px 10px",
                borderRadius: 10,
                border: "1px solid var(--border)",
                background: "var(--card)",
                fontSize: 12,
              }}
            >
              {theme === "dark" ? "☀" : "☾"}
            </button>
          </footer>
        )}

        {!sidebarCollapsed && (
          <div
            className="dt-resize-handle"
            role="separator"
            aria-orientation="vertical"
            aria-label="Resize sidebar"
            onMouseDown={() => {
              resizingRef.current = true;
            }}
          />
        )}
      </aside>

      <div style={{ flex: 1, display: "flex", minWidth: 0, overflow: "hidden" }}>
        <main style={{ flex: 1, display: "flex", flexDirection: "column", minWidth: 0, overflow: "hidden" }}>
          <header className="dt-header">
            <div style={{ display: "flex", alignItems: "center", gap: 10, minWidth: 0 }}>
              <button
                type="button"
                className="dt-mobile-only"
                aria-label="Open navigation drawer"
                onClick={() => setMobileMenuOpen(true)}
                style={{
                  padding: "6px 10px",
                  borderRadius: 10,
                  border: "1px solid var(--border)",
                  background: "var(--card)",
                }}
              >
                ☰
              </button>
              <div className="dt-title-pill">
                {targetKind === "dm" ? (
                  <>
                    <BotAvatar bot={activeBot} size={24} />
                    <strong style={{ fontSize: 13.5 }}>{activeBot.display_name || activeBot.id}</strong>
                    <span style={{ fontSize: 12, color: "var(--muted-foreground)" }}>
                      · {activeBot.role_title || "Coworker"}
                    </span>
                  </>
                ) : (
                  <>
                    <strong style={{ fontSize: 13.5 }}>#{activeChannel.id}</strong>
                    <span style={{ fontSize: 12, color: "var(--muted-foreground)" }}>
                      · {(activeChannel.participants || ["muse"]).map((p) => `@${p}`).join(" ")}
                    </span>
                  </>
                )}
              </div>
            </div>

            <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
              {verifiedProvider && (
                <button
                  type="button"
                  onClick={() => setView("providers")}
                  style={{
                    padding: "5px 11px",
                    borderRadius: 999,
                    border: "1px solid var(--border)",
                    background: "var(--card)",
                    fontSize: 11.5,
                    color: "var(--muted-foreground)",
                    display: "inline-flex",
                    alignItems: "center",
                    gap: 6,
                  }}
                >
                  <span
                    style={{
                      width: 6,
                      height: 6,
                      borderRadius: "50%",
                      background:
                        verifiedProvider.connection_state === "connected"
                          ? "var(--success)"
                          : "var(--warning)",
                    }}
                  />
                  <span>{verifiedProvider.display_name}</span>
                </button>
              )}
              {approvals.length > 0 && (
                <button
                  type="button"
                  onClick={() => setView("chats")}
                  style={{
                    padding: "5px 11px",
                    borderRadius: 999,
                    border: "1px solid var(--warning)",
                    background: "var(--card)",
                    color: "var(--warning)",
                    fontSize: 11.5,
                    fontWeight: 600,
                  }}
                >
                  {approvals.length} Approval{approvals.length > 1 ? "s" : ""} Pending
                </button>
              )}
            </div>
          </header>

          {statusError && (
            <div
              role="alert"
              style={{
                margin: "12px auto 0",
                maxWidth: 960,
                width: "calc(100% - 32px)",
                padding: "10px 14px",
                background: "var(--card)",
                border: "1px solid var(--destructive)",
                borderRadius: 12,
                display: "flex",
                justifyContent: "space-between",
                alignItems: "center",
                gap: 12,
              }}
            >
              <span>{statusError} · Sign in with an operator enrollment token or check daemon connectivity.</span>
              <button
                type="button"
                onClick={() => setStatusError(null)}
                style={{ border: 0, background: "transparent", color: "var(--muted-foreground)" }}
              >
                ✕
              </button>
            </div>
          )}

          {statusNotice && (
            <div
              role="status"
              style={{
                margin: "12px auto 0",
                maxWidth: 960,
                width: "calc(100% - 32px)",
                padding: "8px 14px",
                background: "var(--card)",
                border: "1px solid var(--success)",
                borderRadius: 12,
                display: "flex",
                justifyContent: "space-between",
                alignItems: "center",
              }}
            >
              <span>{statusNotice}</span>
              <button
                type="button"
                onClick={() => setStatusNotice(null)}
                style={{ border: 0, background: "transparent", color: "var(--muted-foreground)" }}
              >
                ✕
              </button>
            </div>
          )}

          <div style={{ flex: 1, overflowY: "auto", padding: "20px 16px" }}>
            <div style={{ maxWidth: 960, margin: "0 auto", width: "100%" }}>
              {loading && (
                <div style={{ padding: 8, color: "var(--muted-foreground)", fontSize: 13 }}>
                  Loading DT workspace…
                </div>
              )}

              {view === "providers" ? (
                <div style={{ display: "flex", flexDirection: "column", gap: 16 }}>
                  <div style={{ display: "flex", justifyContent: "space-between", alignItems: "baseline", flexWrap: "wrap", gap: 8 }}>
                    <div>
                      <h2 style={{ margin: 0, fontSize: 20 }}>Providers, OAuth & Capability Probing</h2>
                      <p style={{ margin: "4px 0 0", color: "var(--muted-foreground)", fontSize: 13 }}>
                        5-tier precedence: <code>Routine → Bot → Workspace → Account → Legacy</code> (fails in place without silent model substitution).
                      </p>
                    </div>
                  </div>

                  {providers.length === 0 ? (
                    <div style={{ padding: 16, background: "var(--card)", borderRadius: 14, border: "1px solid var(--border)" }}>
                      No providers configured yet. Add credentials in Settings or run <code>dt onboard</code>.
                    </div>
                  ) : (
                    <div style={{ display: "grid", gap: 10 }}>
                      {providers.map((p) => {
                        const stages = [
                          { key: "credentials_saved", label: "1. Saved" },
                          { key: "authenticated", label: "2. Authenticated" },
                          { key: "catalog_loaded", label: "3. Catalog" },
                          { key: "connected", label: "4. Verified" },
                        ];
                        return (
                          <div
                            key={p.id}
                            style={{
                              padding: 16,
                              background: "var(--card)",
                              border: "1px solid var(--border)",
                              borderRadius: 14,
                              display: "flex",
                              flexDirection: "column",
                              gap: 10,
                            }}
                          >
                            <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", gap: 8 }}>
                              <div>
                                <strong style={{ fontSize: 15 }}>{p.display_name}</strong>
                                <span style={{ marginLeft: 8, fontSize: 12, color: "var(--muted-foreground)" }}>
                                  ({p.id}{p.selected_model ? ` · ${p.selected_model}` : ""})
                                </span>
                              </div>
                              <span
                                style={{
                                  padding: "3px 10px",
                                  borderRadius: 999,
                                  fontSize: 11.5,
                                  fontWeight: 600,
                                  background: "var(--dialog)",
                                  border: "1px solid var(--border)",
                                  color:
                                    p.connection_state === "connected"
                                      ? "var(--success)"
                                      : p.connection_state === "needs_revalidation"
                                        ? "var(--warning)"
                                        : "var(--muted-foreground)",
                                }}
                              >
                                {p.connection_state}
                              </span>
                            </div>
                            <div style={{ display: "flex", gap: 6, flexWrap: "wrap" }}>
                              {stages.map((st) => {
                                const active = p.connection_state === st.key || p.connection_state === "connected";
                                return (
                                  <span
                                    key={st.key}
                                    style={{
                                      fontSize: 11,
                                      padding: "3px 8px",
                                      borderRadius: 8,
                                      background: active ? "var(--accent)" : "var(--dialog)",
                                      color: active ? "var(--foreground)" : "var(--faint-foreground)",
                                    }}
                                  >
                                    {st.label}
                                  </span>
                                );
                              })}
                            </div>
                          </div>
                        );
                      })}
                    </div>
                  )}

                  <div style={{ marginTop: 8 }}>
                    <h3 style={{ margin: "0 0 8px", fontSize: 16 }}>Connected Apps & Live Capability Probes</h3>
                    <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fill, minmax(210px, 1fr))", gap: 10 }}>
                      {[
                        { id: "gmail", name: "Gmail", scope: "Read / Draft" },
                        { id: "google_calendar", name: "Google Calendar", scope: "Events & Briefs" },
                        { id: "github", name: "GitHub", scope: "PRs & Issues" },
                        { id: "onepassword", name: "1Password CLI", scope: "Vault Reference" },
                      ].map((conn) => (
                        <div
                          key={conn.id}
                          style={{
                            padding: 14,
                            background: "var(--card)",
                            border: "1px solid var(--border)",
                            borderRadius: 12,
                            display: "flex",
                            flexDirection: "column",
                            gap: 8,
                          }}
                        >
                          <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
                            <strong>{conn.name}</strong>
                            <small style={{ color: "var(--muted-foreground)" }}>{conn.scope}</small>
                          </div>
                          <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginTop: 4 }}>
                            <span style={{ fontSize: 11.5, color: "var(--muted-foreground)" }}>
                              {probeResults[conn.id] || "unprobed"}
                            </span>
                            <button
                              type="button"
                              onClick={() => void handleProbeConnector(conn.id)}
                              style={{
                                padding: "4px 10px",
                                borderRadius: 8,
                                border: "1px solid var(--border)",
                                background: "var(--dialog)",
                                fontSize: 11.5,
                              }}
                            >
                              Probe
                            </button>
                          </div>
                        </div>
                      ))}
                    </div>
                  </div>
                </div>
              ) : view === "activity" ? (
                <div style={{ display: "flex", flexDirection: "column", gap: 16 }}>
                  <div>
                    <h2 style={{ margin: 0, fontSize: 20 }}>Scheduled Routines & Proactive Patrols</h2>
                    <p style={{ margin: "4px 0 0", color: "var(--muted-foreground)", fontSize: 13 }}>
                      IANA timezone-aware schedules with quiet-hours suppression and idempotent delivery deduplication.
                    </p>
                  </div>

                  {routines.length === 0 ? (
                    <div style={{ padding: 16, background: "var(--card)", borderRadius: 14, border: "1px solid var(--border)" }}>
                      No scheduled routines configured yet. Use <code>dt routines list</code> or import a team template.
                    </div>
                  ) : (
                    <div style={{ display: "grid", gap: 10 }}>
                      {routines.map((r) => (
                        <div
                          key={r.id}
                          style={{
                            padding: 16,
                            background: "var(--card)",
                            border: "1px solid var(--border)",
                            borderRadius: 14,
                            display: "flex",
                            justifyContent: "space-between",
                            alignItems: "center",
                            flexWrap: "wrap",
                            gap: 12,
                          }}
                        >
                          <div>
                            <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                              <strong>{r.name || r.id}</strong>
                              <span style={{ fontSize: 11.5, color: "var(--muted-foreground)" }}>
                                @{r.bot_id || "muse"} · {r.timezone || "UTC"}
                              </span>
                            </div>
                            <p style={{ margin: "4px 0 0", fontSize: 13, color: "var(--muted-foreground)" }}>
                              {r.instruction || "Scheduled proactive routine"}
                            </p>
                          </div>
                          <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                            {routineTriggerResult[r.id] && (
                              <span
                                style={{
                                  fontSize: 11.5,
                                  padding: "3px 9px",
                                  borderRadius: 999,
                                  background: "var(--dialog)",
                                  border: "1px solid var(--border)",
                                }}
                              >
                                {routineTriggerResult[r.id]}
                              </span>
                            )}
                            <button
                              type="button"
                              onClick={() => void handleTriggerRoutine(r.id)}
                              style={{
                                padding: "6px 12px",
                                borderRadius: 10,
                                border: "1px solid var(--border)",
                                background: "var(--dialog)",
                                fontSize: 12.5,
                                fontWeight: 550,
                              }}
                            >
                              Trigger Now
                            </button>
                          </div>
                        </div>
                      ))}
                    </div>
                  )}
                </div>
              ) : view === "spaces" ? (
                <div style={{ display: "flex", flexDirection: "column", gap: 16 }}>
                  <div>
                    <h2 style={{ margin: 0, fontSize: 20 }}>Pages & Token-Isolated Static Spaces</h2>
                    <p style={{ margin: "4px 0 0", color: "var(--muted-foreground)", fontSize: 13 }}>
                      Served from a separate loopback origin inside a strict <code>sandbox="allow-scripts"</code> frame.
                    </p>
                  </div>

                  <form
                    onSubmit={(e) => void handleCreateSpace(e)}
                    style={{
                      display: "flex",
                      flexWrap: "wrap",
                      gap: 8,
                      padding: 14,
                      background: "var(--card)",
                      border: "1px solid var(--border)",
                      borderRadius: 14,
                    }}
                  >
                    <input
                      value={newSpaceSlug}
                      onChange={(e) => setNewSpaceSlug(e.target.value)}
                      placeholder="Slug (e.g. sprint-dashboard)"
                      aria-label="Page slug"
                      style={{
                        flex: "1 1 180px",
                        padding: "8px 12px",
                        borderRadius: 10,
                        border: "1px solid var(--border)",
                        background: "var(--dialog)",
                      }}
                    />
                    <input
                      value={newSpaceTitle}
                      onChange={(e) => setNewSpaceTitle(e.target.value)}
                      placeholder="Page title"
                      aria-label="Page title"
                      style={{
                        flex: "2 1 240px",
                        padding: "8px 12px",
                        borderRadius: 10,
                        border: "1px solid var(--border)",
                        background: "var(--dialog)",
                      }}
                    />
                    <button
                      type="submit"
                      style={{
                        padding: "8px 16px",
                        borderRadius: 10,
                        border: 0,
                        background: "var(--primary)",
                        color: "var(--primary-foreground)",
                        fontWeight: 600,
                      }}
                    >
                      New Page
                    </button>
                  </form>

                  {spaces.length === 0 ? (
                    <div style={{ padding: 16, background: "var(--card)", borderRadius: 14, border: "1px solid var(--border)" }}>
                      No published Pages yet. Create one above or ask @muse to generate an interactive Space.
                    </div>
                  ) : (
                    <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fill, minmax(250px, 1fr))", gap: 12 }}>
                      {spaces.map((s) => (
                        <div
                          key={s.slug}
                          style={{
                            padding: 16,
                            background: "var(--card)",
                            border: "1px solid var(--border)",
                            borderRadius: 14,
                            display: "flex",
                            flexDirection: "column",
                            justifyContent: "space-between",
                            gap: 12,
                          }}
                        >
                          <div>
                            <strong style={{ fontSize: 15 }}>{s.title || s.slug}</strong>
                            <small style={{ display: "block", color: "var(--muted-foreground)", marginTop: 2 }}>
                              sandbox://{s.slug}
                            </small>
                          </div>
                          <button
                            type="button"
                            onClick={() => void handleOpenSplitSpace(s.slug)}
                            style={{
                              padding: "7px 12px",
                              borderRadius: 10,
                              border: "1px solid var(--border)",
                              background: "var(--dialog)",
                              fontSize: 12.5,
                              fontWeight: 550,
                            }}
                          >
                            Open Split View
                          </button>
                        </div>
                      ))}
                    </div>
                  )}
                </div>
              ) : view === "doctor" ? (
                <div style={{ display: "flex", flexDirection: "column", gap: 16 }}>
                  <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", flexWrap: "wrap", gap: 8 }}>
                    <div>
                      <h2 style={{ margin: 0, fontSize: 20 }}>DT Doctor Status (Sanitized)</h2>
                      <p style={{ margin: "4px 0 0", color: "var(--muted-foreground)", fontSize: 13 }}>
                        Unified readiness engine across storage leases, SQLite integrity, IPC handshakes, and permissions.
                      </p>
                    </div>
                    <button
                      type="button"
                      onClick={() => setShowRawDoctor((v) => !v)}
                      style={{
                        padding: "6px 12px",
                        borderRadius: 10,
                        border: "1px solid var(--border)",
                        background: "var(--card)",
                        fontSize: 12,
                      }}
                    >
                      {showRawDoctor ? "Hide Raw JSON" : "Inspect JSON"}
                    </button>
                  </div>

                  <div
                    style={{
                      padding: 16,
                      background: "var(--card)",
                      border: "1px solid var(--border)",
                      borderRadius: 14,
                      display: "flex",
                      justifyContent: "space-between",
                      alignItems: "center",
                    }}
                  >
                    <div>
                      <small style={{ color: "var(--muted-foreground)", textTransform: "uppercase", fontSize: 11 }}>
                        Installation Readiness
                      </small>
                      <div style={{ fontSize: 18, fontWeight: 650, marginTop: 2 }}>
                        {String(doctor?.installation_readiness ?? "ready")}
                      </div>
                    </div>
                    <span
                      style={{
                        padding: "4px 12px",
                        borderRadius: 999,
                        background: "var(--dialog)",
                        border: "1px solid var(--border)",
                        color: "var(--success)",
                        fontSize: 12,
                        fontWeight: 600,
                      }}
                    >
                      Schema v{String(doctor?.schema ?? 1)} · Sanitized
                    </span>
                  </div>

                  {doctorCapabilities.length > 0 && (
                    <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fill, minmax(240px, 1fr))", gap: 10 }}>
                      {doctorCapabilities.map((cap) => (
                        <div
                          key={cap.id}
                          style={{
                            padding: 14,
                            background: "var(--card)",
                            border: "1px solid var(--border)",
                            borderRadius: 12,
                          }}
                        >
                          <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
                            <strong>{cap.name}</strong>
                            <span style={{ fontSize: 11, color: "var(--muted-foreground)" }}>{cap.readiness}</span>
                          </div>
                          <p style={{ margin: "6px 0 0", fontSize: 12, color: "var(--muted-foreground)" }}>{cap.summary}</p>
                        </div>
                      ))}
                    </div>
                  )}

                  {doctorChecks.length > 0 && (
                    <div style={{ display: "grid", gap: 8 }}>
                      {doctorChecks.map((chk) => (
                        <div
                          key={chk.id}
                          style={{
                            padding: "11px 14px",
                            background: "var(--card)",
                            border: "1px solid var(--border)",
                            borderRadius: 12,
                            display: "flex",
                            justifyContent: "space-between",
                            alignItems: "center",
                            gap: 12,
                          }}
                        >
                          <div>
                            <strong>{chk.id}</strong>
                            <span style={{ marginLeft: 8, fontSize: 12, color: "var(--muted-foreground)" }}>
                              {chk.summary}
                            </span>
                          </div>
                          <span style={{ fontSize: 11.5, fontWeight: 600, color: chk.outcome === "pass" ? "var(--success)" : "var(--warning)" }}>
                            {chk.outcome}
                          </span>
                        </div>
                      ))}
                    </div>
                  )}

                  {(showRawDoctor || doctorChecks.length === 0) && (
                    <pre
                      style={{
                        background: "var(--card)",
                        border: "1px solid var(--border)",
                        padding: 14,
                        borderRadius: 12,
                        overflow: "auto",
                        fontSize: 12,
                      }}
                    >
                      {JSON.stringify(doctor ?? { status: "unavailable" }, null, 2)}
                    </pre>
                  )}
                </div>
              ) : view === "settings" ? (
                <div style={{ display: "flex", flexDirection: "column", gap: 16 }}>
                  <h2 style={{ margin: 0, fontSize: 20 }}>Settings & Hosted Session</h2>
                  <div
                    style={{
                      padding: 16,
                      background: "var(--card)",
                      border: "1px solid var(--border)",
                      borderRadius: 14,
                      display: "flex",
                      flexDirection: "column",
                      gap: 12,
                    }}
                  >
                    <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
                      <button
                        type="button"
                        onClick={() => setTheme(theme === "dark" ? "light" : "dark")}
                        style={{
                          padding: "8px 14px",
                          borderRadius: 10,
                          border: "1px solid var(--border)",
                          background: "var(--dialog)",
                        }}
                      >
                        Switch to {theme === "dark" ? "Light" : "Dark"} Theme
                      </button>
                      <button
                        type="button"
                        onClick={() =>
                          void client.signOut().then(() => setStatusNotice("Signed out cleanly."))
                        }
                        style={{
                          padding: "8px 14px",
                          borderRadius: 10,
                          border: "1px solid var(--border)",
                          background: "var(--dialog)",
                        }}
                      >
                        Sign out
                      </button>
                    </div>
                    <form onSubmit={(e) => void handleEnrollSignIn(e)} style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
                      <input
                        value={enrollmentInput}
                        onChange={(e) => setEnrollmentInput(e.target.value)}
                        placeholder="Operator enrollment token (redeemed for HttpOnly; Secure cookie + CSRF)"
                        style={{
                          flex: "1 1 260px",
                          padding: "9px 12px",
                          borderRadius: 10,
                          background: "var(--dialog)",
                          color: "var(--foreground)",
                          border: "1px solid var(--border)",
                        }}
                      />
                      <button
                        type="submit"
                        style={{
                          padding: "9px 16px",
                          borderRadius: 10,
                          border: 0,
                          background: "var(--primary)",
                          color: "var(--primary-foreground)",
                          fontWeight: 600,
                        }}
                      >
                        Connect Workspace
                      </button>
                    </form>
                  </div>
                </div>
              ) : (
                <div style={{ display: "flex", flexDirection: "column", gap: 14 }}>
                  {approvals.map((a) => (
                    <div
                      key={a.request_id}
                      style={{
                        padding: 16,
                        background: "var(--card)",
                        border: "1px solid var(--warning)",
                        borderRadius: 16,
                        display: "flex",
                        flexDirection: "column",
                        gap: 8,
                      }}
                    >
                      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
                        <strong style={{ color: "var(--warning)" }}>⚡ Inline Approval Required · {a.title}</strong>
                        <small style={{ color: "var(--muted-foreground)" }}>Non-blocking</small>
                      </div>
                      <p style={{ margin: 0, fontSize: 13.5 }}>{a.details}</p>
                      <div style={{ display: "flex", gap: 8, marginTop: 4 }}>
                        <button
                          type="button"
                          onClick={() => void handleApproval(a.request_id, true)}
                          style={{
                            padding: "6px 14px",
                            borderRadius: 10,
                            border: 0,
                            background: "var(--success)",
                            color: "#fff",
                            fontWeight: 600,
                          }}
                        >
                          Approve
                        </button>
                        <button
                          type="button"
                          onClick={() => void handleApproval(a.request_id, false)}
                          style={{
                            padding: "6px 14px",
                            borderRadius: 10,
                            border: "1px solid var(--border)",
                            background: "var(--dialog)",
                          }}
                        >
                          Deny
                        </button>
                      </div>
                    </div>
                  ))}

                  {messages.length === 0 ? (
                    <div
                      style={{
                        padding: "32px 20px",
                        background: "var(--card)",
                        border: "1px solid var(--border)",
                        borderRadius: 20,
                        textAlign: "center",
                        display: "flex",
                        flexDirection: "column",
                        alignItems: "center",
                        gap: 12,
                      }}
                    >
                      {targetKind === "dm" && <BotAvatar bot={activeBot} size={46} />}
                      <div>
                        <strong style={{ fontSize: 16, display: "block" }}>
                          {targetKind === "dm"
                            ? `Start a thread with @${selectedTarget}`
                            : `Welcome to #${selectedTarget}`}
                        </strong>
                        <p style={{ margin: "4px 0 0", color: "var(--muted-foreground)", fontSize: 13 }}>
                          Conversations are non-blocking: approvals and background tasks stream inline while you keep typing.
                        </p>
                      </div>
                      <div style={{ display: "flex", flexWrap: "wrap", justifyContent: "center", gap: 8, marginTop: 6 }}>
                        {(STARTER_PROMPTS[selectedTarget] || STARTER_PROMPTS.muse).map((item) => (
                          <button
                            key={item.label}
                            type="button"
                            onClick={() => updateDraft(item.prompt)}
                            style={{
                              padding: "7px 13px",
                              borderRadius: 999,
                              border: "1px solid var(--border)",
                              background: "var(--dialog)",
                              fontSize: 12.5,
                            }}
                          >
                            ✦ {item.label}
                          </button>
                        ))}
                      </div>
                    </div>
                  ) : (
                    messages.map((m) => {
                      const author = m.sender || m.author || selectedTarget;
                      const isUser = author === "user" || author === "you";
                      return (
                        <div
                          key={m.id}
                          style={{
                            alignSelf: isUser ? "flex-end" : "flex-start",
                            maxWidth: "82%",
                            minWidth: 160,
                            padding: "12px 16px",
                            background: isUser ? "var(--chat-user)" : "var(--card)",
                            color: isUser ? "var(--chat-user-foreground)" : "var(--foreground)",
                            border: "1px solid var(--border)",
                            borderRadius: 18,
                          }}
                        >
                          <div style={{ display: "flex", justifyContent: "space-between", gap: 12, marginBottom: 4 }}>
                            <small style={{ fontWeight: 600, fontSize: 11.5, opacity: 0.85 }}>{author}</small>
                            {m.created_at && (
                              <small style={{ fontSize: 10.5, opacity: 0.6 }}>{m.created_at}</small>
                            )}
                          </div>
                          <div style={{ whiteSpace: "pre-wrap", overflowWrap: "anywhere", fontSize: 14.5, lineHeight: "21px" }}>
                            {m.content}
                          </div>
                        </div>
                      );
                    })
                  )}
                </div>
              )}
            </div>
          </div>

          {view === "chats" && (
            <div className="dt-composer">
              <div style={{ display: "flex", gap: 6, flexWrap: "wrap", marginBottom: 6 }}>
                {[
                  { label: "/do", insert: "/do " },
                  { label: "@researcher", insert: "@researcher " },
                  { label: "@coder", insert: "@coder " },
                  { label: "→ arrow", insert: `${draft ? `${draft} -> ` : "-> "}` },
                ].map((chip) => (
                  <button
                    key={chip.label}
                    type="button"
                    onClick={() =>
                      updateDraft(
                        chip.label === "→ arrow"
                          ? chip.insert
                          : `${draft}${draft && !draft.endsWith(" ") ? " " : ""}${chip.insert}`,
                      )
                    }
                    style={{
                      padding: "2px 9px",
                      borderRadius: 999,
                      border: "1px solid var(--border)",
                      background: "var(--dialog)",
                      fontSize: 11,
                      color: "var(--muted-foreground)",
                    }}
                  >
                    {chip.label}
                  </button>
                ))}
              </div>
              <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
                <input
                  value={draft}
                  onChange={(e) => updateDraft(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" && !e.shiftKey) {
                      e.preventDefault();
                      void send();
                    }
                  }}
                  placeholder={
                    targetKind === "dm"
                      ? `Message @${selectedTarget} (type -> for →, Enter to send)…`
                      : `Post to #${selectedTarget} (@mention any bot)…`
                  }
                  style={{
                    flex: 1,
                    padding: "8px 6px",
                    background: "transparent",
                    color: "var(--foreground)",
                    border: 0,
                    outline: "none",
                    fontSize: 14.5,
                  }}
                />
                <button
                  type="button"
                  onClick={() => void send()}
                  aria-label="Send"
                  style={{
                    padding: "8px 16px",
                    borderRadius: 999,
                    border: 0,
                    background: "var(--primary)",
                    color: "var(--primary-foreground)",
                    fontWeight: 600,
                    fontSize: 13,
                  }}
                >
                  Send
                </button>
              </div>
            </div>
          )}
        </main>

        {splitSpaceSlug && (
          <section
            aria-label="Split Space Preview"
            style={{
              width: "44%",
              minWidth: 320,
              borderLeft: "1px solid var(--border)",
              background: "var(--dialog)",
              display: "flex",
              flexDirection: "column",
            }}
          >
            <header
              style={{
                minHeight: 64,
                padding: "10px 16px",
                borderBottom: "1px solid var(--border)",
                display: "flex",
                justifyContent: "space-between",
                alignItems: "center",
              }}
            >
              <div>
                <strong>Page · {splitSpaceSlug}</strong>
                <small style={{ display: "block", color: "var(--muted-foreground)", fontSize: 11 }}>
                  Isolated origin · sandbox="allow-scripts"
                </small>
              </div>
              <button
                type="button"
                onClick={() => {
                  setSplitSpaceSlug(null);
                  setSplitSpaceUrl(null);
                }}
                style={{
                  padding: "5px 10px",
                  borderRadius: 8,
                  border: "1px solid var(--border)",
                  background: "var(--card)",
                }}
              >
                Close
              </button>
            </header>
            {splitSpaceUrl ? (
              <iframe
                title={`Isolated Space ${splitSpaceSlug}`}
                src={splitSpaceUrl}
                sandbox="allow-scripts"
                style={{ flex: 1, width: "100%", border: 0, background: "#fff" }}
              />
            ) : (
              <div style={{ padding: 20, color: "var(--muted-foreground)" }}>Loading isolated Page…</div>
            )}
          </section>
        )}
      </div>

      {commandOpen && (
        <div
          role="dialog"
          aria-modal="true"
          aria-label="Quick Jump Command Palette"
          onClick={() => setCommandOpen(false)}
          style={{
            position: "fixed",
            inset: 0,
            zIndex: 50,
            background: "rgba(0, 0, 0, 0.6)",
            display: "grid",
            placeItems: "start center",
            paddingTop: "12vh",
          }}
        >
          <div
            onClick={(e) => e.stopPropagation()}
            style={{
              width: "min(540px, 92vw)",
              background: "var(--dialog)",
              border: "1px solid var(--input)",
              borderRadius: 16,
              padding: 14,
              boxShadow: "0 24px 80px rgba(0, 0, 0, 0.55)",
              display: "flex",
              flexDirection: "column",
              gap: 10,
            }}
          >
            <input
              autoFocus
              value={commandQuery}
              onChange={(e) => setCommandQuery(e.target.value)}
              placeholder="Jump to @bot, #channel, Doctor, Pages, or action…"
              aria-label="Filter commands"
              style={{
                width: "100%",
                padding: "10px 12px",
                borderRadius: 10,
                border: "1px solid var(--border)",
                background: "var(--card)",
                fontSize: 14,
              }}
            />
            <div style={{ maxHeight: 320, overflowY: "auto", display: "flex", flexDirection: "column", gap: 4 }}>
              {commandItems.map((item) => (
                <button
                  key={item.id}
                  type="button"
                  onClick={() => {
                    item.run();
                    setCommandOpen(false);
                  }}
                  style={{
                    padding: "9px 12px",
                    borderRadius: 10,
                    border: "1px solid transparent",
                    background: "var(--card)",
                    textAlign: "left",
                    display: "flex",
                    justifyContent: "space-between",
                    alignItems: "center",
                  }}
                >
                  <span>{item.label}</span>
                  <small style={{ color: "var(--muted-foreground)", fontSize: 11 }}>{item.category}</small>
                </button>
              ))}
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
