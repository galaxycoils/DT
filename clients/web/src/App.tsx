import React, { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  applySmartArrowEdit,
  createClientMessageId,
  DtV1Client,
  parseMessageCard,
  type ActivityPost,
  type Channel,
  type ConnectionMode,
  type ConnectorSummary,
  type DirectMessage,
  type GoalItem,
  type MarketplaceListing,
  type ProviderProfile,
  type Routine,
  type RoutineRun,
  type Task,
} from "./api";
import "./layout.css";

interface PendingApproval {
  request_id: string;
  title: string;
  details: string;
  tier?: string;
}

interface FailedReplyBanner {
  clientMsgId: string;
  content: string;
  detail: string;
  providerSetupRequired: boolean;
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

interface SpaceSummary {
  slug: string;
  title?: string;
  published?: boolean;
}

export type ViewTab =
  | "chats"
  | "activity"
  | "goals"
  | "routines"
  | "spaces"
  | "providers"
  | "connectors"
  | "marketplace"
  | "billing"
  | "doctor"
  | "settings";

export const DRAFT_STORAGE_KEY = "dt-web-drafts-v1-scoped";

const WORKSPACE_REFRESH_EVENTS = [
  "open",
  "DIRECT_MESSAGE_ADDED",
  "CARD_UPDATED",
  "STATE_INVALIDATED",
  "TASK_UPDATED",
  "ROUTINE_UPDATED",
  "ROUTINE_QUEUED",
  "BOT_UPDATED",
  "CHANNEL_UPDATED",
  "RESYNC_REQUIRED",
];

const NAV_TABS: Array<{ id: ViewTab; label: string; icon: string }> = [
  { id: "chats", label: "Chats", icon: "💬" },
  { id: "activity", label: "Activity", icon: "📰" },
  { id: "goals", label: "Goals", icon: "🎯" },
  { id: "routines", label: "Routines", icon: "⚡" },
  { id: "spaces", label: "Pages", icon: "◧" },
  { id: "providers", label: "Providers", icon: "◈" },
  { id: "connectors", label: "Apps", icon: "🔌" },
  { id: "marketplace", label: "Marketplace", icon: "🛍" },
  { id: "billing", label: "Billing", icon: "💳" },
  { id: "doctor", label: "Doctor", icon: "✚" },
  { id: "settings", label: "Settings", icon: "⚙" },
];

const DEFAULT_BOTS: BotSummary[] = [
  { id: "muse", display_name: "Muse", role_title: "Primary Personal Agent", avatar_color: "#4D9BFF", avatar_shape: "spark", status_phrase: "Ready for tasks" },
  { id: "researcher", display_name: "Researcher", role_title: "Wide Research & Citations", avatar_color: "#00BA69", avatar_shape: "hex", status_phrase: "Provenance indexed" },
  { id: "coder", display_name: "Coder", role_title: "Isolated Worktree & Patch", avatar_color: "#E28600", avatar_shape: "rounded_square", status_phrase: "VM sandbox idle" },
  { id: "guardian", display_name: "Guardian", role_title: "Safety & Discretion Audit", avatar_color: "#9B6BFF", avatar_shape: "diamond", status_phrase: "Zero policy alerts" },
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
    { label: "Test coverage", prompt: "Audit our test suite against the coverage inventory." },
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

  const [sessionUser, setSessionUser] = useState<string | null>(null);
  const [connectionMode, setConnectionMode] = useState<ConnectionMode | null>(null);
  const [connectionAttempt, setConnectionAttempt] = useState(0);
  const sessionChecked = connectionMode !== null;
  const [workspaceId, setWorkspaceId] = useState(client.getWorkspaceId());
  const [credentialInput, setCredentialInput] = useState("");
  const [signInBusy, setSignInBusy] = useState(false);

  const [bots, setBots] = useState<BotSummary[]>([]);
  const [channels, setChannels] = useState<Channel[]>([]);
  const [chatKind, setChatKind] = useState<"dm" | "channel">("dm");
  const [selectedBot, setSelectedBot] = useState("muse");
  const [selectedChannel, setSelectedChannel] = useState("general");
  const [newChannelId, setNewChannelId] = useState("");
  const [newChannelTopic, setNewChannelTopic] = useState("");

  const [drafts, setDrafts] = useState<Record<string, string>>({});
  const [messages, setMessages] = useState<DirectMessage[]>([]);
  const [approvals, setApprovals] = useState<PendingApproval[]>([]);
  const [sending, setSending] = useState(false);
  const [pendingClientMsgId, setPendingClientMsgId] = useState<string | null>(null);
  const [failedReply, setFailedReply] = useState<FailedReplyBanner | null>(null);

  const [posts, setPosts] = useState<ActivityPost[]>([]);
  const [newPostTitle, setNewPostTitle] = useState("");
  const [newPostBody, setNewPostBody] = useState("");
  const [tasks, setTasks] = useState<Task[]>([]);

  const [goals, setGoals] = useState<GoalItem[]>([]);
  const [newGoalTitle, setNewGoalTitle] = useState("");
  const [newGoalDesc, setNewGoalDesc] = useState("");

  const [routines, setRoutines] = useState<Routine[]>([]);
  const [selectedRoutineId, setSelectedRoutineId] = useState<string | null>(null);
  const [routineRuns, setRoutineRuns] = useState<RoutineRun[]>([]);
  const [routineTriggerResult, setRoutineTriggerResult] = useState<Record<string, string>>({});
  const [newRoutineName, setNewRoutineName] = useState("");
  const [newRoutineInstruction, setNewRoutineInstruction] = useState("");

  const [spaces, setSpaces] = useState<SpaceSummary[]>([]);
  const [splitSpaceSlug, setSplitSpaceSlug] = useState<string | null>(null);
  const [splitSpacePreview, setSplitSpacePreview] = useState<{ url: string } | { error: string } | null>(null);
  const [newSpaceSlug, setNewSpaceSlug] = useState("");
  const [newSpaceTitle, setNewSpaceTitle] = useState("");

  const [files, setFiles] = useState<Array<{ path: string; size: number }>>([]);
  const [filePreview, setFilePreview] = useState<Awaited<ReturnType<DtV1Client["previewFile"]>> | null>(null);

  const [providers, setProviders] = useState<ProviderProfile[]>([]);
  const [providerKeyDrafts, setProviderKeyDrafts] = useState<Record<string, string>>({});
  const [connectors, setConnectors] = useState<ConnectorSummary[]>([]);
  const [probeResults, setProbeResults] = useState<Record<string, string>>({});
  const [marketplace, setMarketplace] = useState<{
    catalog: MarketplaceListing[];
    installed: MarketplaceListing[];
  }>({ catalog: [], installed: [] });
  const [billing, setBilling] = useState<Record<string, unknown> | null>(null);
  const [doctor, setDoctor] = useState<Record<string, unknown> | null>(null);
  const [showRawDoctor, setShowRawDoctor] = useState(false);

  const [refreshVersion, setRefreshVersion] = useState(0);
  const [statusError, setStatusError] = useState<string | null>(null);
  const [statusNotice, setStatusNotice] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  const resizingRef = useRef(false);
  const sessionEpochRef = useRef(0);
  const eventStreamRef = useRef<AbortController | null>(null);

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
          setSplitSpacePreview(null);
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

  const activeTarget = chatKind === "channel" ? `channel:${selectedChannel}` : `dm:${selectedBot}`;
  const draftKey = JSON.stringify([sessionUser, workspaceId, activeTarget]);
  const draft = drafts[draftKey] ?? "";

  const clearWorkspaceState = useCallback(() => {
    sessionEpochRef.current += 1;
    eventStreamRef.current?.abort();
    eventStreamRef.current = null;
    client.abortAllRequests();
    setBots([]);
    setChannels([]);
    setMessages([]);
    setApprovals([]);
    setPosts([]);
    setTasks([]);
    setGoals([]);
    setRoutines([]);
    setSelectedRoutineId(null);
    setRoutineRuns([]);
    setRoutineTriggerResult({});
    setSpaces([]);
    setSplitSpaceSlug(null);
    setSplitSpacePreview(null);
    setFiles([]);
    setFilePreview(null);
    setProviders([]);
    setProviderKeyDrafts({});
    setConnectors([]);
    setProbeResults({});
    setMarketplace({ catalog: [], installed: [] });
    setBilling(null);
    setDoctor(null);
    setFailedReply(null);
    setPendingClientMsgId(null);
    setStatusError(null);
    setStatusNotice(null);
    setLoading(false);
    setMobileMenuOpen(false);
    setCommandOpen(false);
  }, [client]);

  const selectConversation = useCallback(
    (kind: "dm" | "channel", id: string) => {
      const nextTarget = `${kind}:${id}`;
      const nextDraftKey = JSON.stringify([sessionUser, workspaceId, nextTarget]);
      if (drafts[nextDraftKey] === undefined) {
        const restored = loadTargetDraft(getBrowserStorage(), nextDraftKey);
        if (restored) {
          setDrafts((prev) => ({ ...prev, [nextDraftKey]: restored }));
        }
      }
      setView("chats");
      setChatKind(kind);
      if (kind === "dm") {
        setSelectedBot(id);
        setFiles([]);
        setFilePreview(null);
      } else {
        setSelectedChannel(id);
      }
      setMessages([]);
      setFailedReply(null);
      setPendingClientMsgId(null);
      setMobileMenuOpen(false);
    },
    [drafts, sessionUser, workspaceId],
  );

  useEffect(() => {
    try {
      const storage = getBrowserStorage();
      const raw = storage?.getItem(DRAFT_STORAGE_KEY);
      if (raw) setDrafts(JSON.parse(raw) as Record<string, string>);
    } catch {
      // Storage may be unavailable.
    }
    const controller = new AbortController();
    void client
      .connectionMode(controller.signal)
      .then(async (mode) => {
        const session = mode === "hosted"
          ? await client.restoreSession(controller.signal).catch(() => null)
          : null;
        if (controller.signal.aborted) return;
        if (session) {
          setWorkspaceId(session.ws_id);
          setSessionUser(session.user_id);
        }
        setConnectionMode(mode);
      })
      .catch((err) => {
        if (!controller.signal.aborted) {
          setStatusError(err instanceof Error ? err.message : "Could not reach DT");
        }
      });
    return () => controller.abort();
  }, [client, connectionAttempt]);

  function updateDraft(value: string, raw = false) {
    const transformed = raw ? value : applySmartArrowEdit(draft, value);
    const next = { ...drafts, [draftKey]: transformed };
    setDrafts(next);
    if (!transformed.trim()) setPendingClientMsgId(null);
    saveTargetDraft(getBrowserStorage(), draftKey, transformed);
  }

  useEffect(() => {
    if (!sessionChecked || !sessionUser) return;
    const epoch = sessionEpochRef.current;
    const controller = new AbortController();
    setLoading(true);
    setStatusError(null);
    void Promise.all([
      client.bootstrap(controller.signal),
      client.getProviders(controller.signal).catch(() => ({ profiles: [] })),
      client.getDoctorStatus(controller.signal).catch(() => null),
      client.listSpaces(controller.signal).catch(() => []),
      client.listChannels(controller.signal).catch(() => []),
    ])
      .then(([snap, prov, doc, sp, ch]) => {
        if (controller.signal.aborted || epoch !== sessionEpochRef.current) return;
        if (Array.isArray(snap.bots)) {
          const statuses =
            snap.bot_statuses && typeof snap.bot_statuses === "object"
              ? (snap.bot_statuses as Record<string, { phrase?: string | null; needs_attention?: boolean }>)
              : {};
          setBots(
            (snap.bots as BotSummary[]).map((b) => {
              const st = statuses[b.id];
              return st
                ? {
                    ...b,
                    status_phrase: st.phrase ?? b.status_phrase,
                    needs_attention: st.needs_attention ?? b.needs_attention,
                  }
                : b;
            }),
          );
        }
        if (Array.isArray(snap.pending_approvals)) {
          setApprovals(snap.pending_approvals as PendingApproval[]);
        }
        if (Array.isArray(snap.integrations)) {
          setConnectors(snap.integrations as ConnectorSummary[]);
        }
        if (Array.isArray(prov.profiles)) {
          setProviders(prov.profiles);
        }
        if (doc) setDoctor(doc);
        if (Array.isArray(sp)) setSpaces(sp);
        if (Array.isArray(ch)) setChannels(ch);
      })
      .catch((err) => {
        if (controller.signal.aborted || epoch !== sessionEpochRef.current) return;
        setStatusError(err instanceof Error ? err.message : "Failed to load DT workspace");
      })
      .finally(() => {
        if (!controller.signal.aborted && epoch === sessionEpochRef.current) {
          setLoading(false);
        }
      });
    return () => controller.abort();
  }, [client, sessionChecked, sessionUser, workspaceId, refreshVersion]);

  useEffect(() => {
    if (!sessionChecked || !sessionUser) return;
    const controller = new AbortController();
    eventStreamRef.current = controller;
    const refresh = () => setRefreshVersion((version) => version + 1);
    if (connectionMode === "local") {
      void client.subscribeLocalEvents(
        controller.signal,
        (name) => {
          if (WORKSPACE_REFRESH_EVENTS.includes(name)) refresh();
        },
        () => {
          void client.signOut();
          clearWorkspaceState();
          setSessionUser(null);
          setStatusError("The Companion token was rejected. Connect again with the current token.");
        },
      );
      return () => controller.abort();
    }
    const seenSequences = new Set<string>();
    const invalidated = (ev?: Event) => {
      const msg = ev as MessageEvent | undefined;
      if (msg?.lastEventId) {
        if (seenSequences.has(msg.lastEventId)) return;
        seenSequences.add(msg.lastEventId);
      }
      refresh();
    };
    const events = new EventSource(client.eventsUrl(), { withCredentials: true });
    for (const name of WORKSPACE_REFRESH_EVENTS) {
      events.addEventListener(name, invalidated);
    }
    controller.signal.addEventListener("abort", () => events.close(), { once: true });
    return () => controller.abort();
  }, [clearWorkspaceState, client, connectionMode, sessionChecked, sessionUser, workspaceId]);

  useEffect(() => {
    if (!splitSpaceSlug) return;
    if (connectionMode !== "local") {
      setSplitSpacePreview({ error: "Open DT on the computer running it to preview this Page." });
      return;
    }
    const controller = new AbortController();
    void client.getSpaceUrl(splitSpaceSlug, controller.signal)
      .then(({ url, preview_parent_origin: origin }) => {
        if (controller.signal.aborted) return;
        if (!origin) {
          setSplitSpacePreview({ error: "Update the DT daemon to v1.0.5 or later to preview Pages." });
        } else if (origin !== window.location.origin) {
          setSplitSpacePreview({ error: `Open DT at ${origin}/app/ to preview this Page.` });
        } else {
          setSplitSpacePreview({ url });
        }
      })
      .catch(() => {
        if (!controller.signal.aborted) {
          setSplitSpacePreview({ error: "Could not load this Page preview. Close it and try again." });
        }
      });
    return () => controller.abort();
  }, [client, connectionMode, splitSpaceSlug]);

  useEffect(() => {
    if (!sessionChecked || !sessionUser) return;
    const epoch = sessionEpochRef.current;
    const controller = new AbortController();
    const isStale = () => controller.signal.aborted || epoch !== sessionEpochRef.current;

    if (view === "chats") {
      const loader =
        chatKind === "channel"
          ? client.listChannelMessages(selectedChannel, controller.signal)
          : client.listDirectMessages(selectedBot, controller.signal);
      void loader
        .then((items) => {
          if (!isStale()) setMessages(Array.isArray(items) ? items : []);
        })
        .catch((error) => {
          if (!isStale()) setStatusError(String(error));
        });
    } else if (view === "routines") {
      void client
        .listRoutines(controller.signal)
        .then((items) => {
          if (!isStale()) setRoutines(items);
        })
        .catch((error) => {
          if (!isStale()) setStatusError(String(error));
        });
    } else if (view === "activity") {
      void client
        .listTasks(controller.signal)
        .then((items) => {
          if (!isStale()) setTasks(Array.isArray(items) ? items : []);
        })
        .catch((error) => {
          if (!isStale()) setStatusError(String(error));
        });
      void client
        .listActivity(controller.signal)
        .then((data) => {
          if (!isStale()) setPosts(data.posts);
        })
        .catch((error) => {
          if (!isStale()) setStatusError(String(error));
        });
    } else if (view === "goals") {
      void client
        .listGoals(controller.signal)
        .then((items) => {
          if (!isStale()) setGoals(Array.isArray(items) ? items : []);
        })
        .catch((error) => {
          if (!isStale()) setStatusError(String(error));
        });
    } else if (view === "connectors") {
      void client
        .listConnectors(controller.signal)
        .then((data) => {
          if (!isStale()) {
            const items = Array.isArray(data?.integrations)
              ? data.integrations
              : Array.isArray(data)
                ? (data as ConnectorSummary[])
                : [];
            if (items.length > 0) {
              setConnectors(items);
            }
          }
        })
        .catch((error) => {
          if (!isStale()) setStatusError(String(error));
        });
    } else if (view === "marketplace") {
      void client
        .listMarketplace(controller.signal)
        .then((data) => {
          if (!isStale()) {
            setMarketplace({
              catalog: Array.isArray(data?.catalog) ? data.catalog : [],
              installed: Array.isArray(data?.installed) ? data.installed : [],
            });
          }
        })
        .catch((error) => {
          if (!isStale()) setStatusError(String(error));
        });
    } else if (view === "billing") {
      void client
        .getBillingStatus(controller.signal)
        .then((data) => {
          if (!isStale()) setBilling(data);
        })
        .catch((error) => {
          if (!isStale()) setStatusError(String(error));
        });
    }

    return () => controller.abort();
  }, [
    chatKind,
    client,
    refreshVersion,
    selectedBot,
    selectedChannel,
    sessionChecked,
    sessionUser,
    view,
    workspaceId,
  ]);

  async function dispatchDirect(content: string, existingClientMsgId?: string, clearDraft = true) {
    const clientMsgId = existingClientMsgId || pendingClientMsgId || createClientMessageId();
    setPendingClientMsgId(clientMsgId);
    setStatusError(null);
    setSending(true);
    try {
      const outcome = await client.postDirectMessage(selectedBot, content, clientMsgId);
      if (clearDraft) updateDraft("", true);
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
      setRefreshVersion((version) => version + 1);
    } catch (err) {
      setStatusError(err instanceof Error ? err.message : "Failed to send message. Draft preserved.");
    } finally {
      setSending(false);
    }
  }

  async function send(overridePrompt?: string) {
    const trimmed = (overridePrompt ?? draft).trim();
    if (!trimmed || sending) return;
    if (chatKind === "channel") {
      setStatusError(null);
      setSending(true);
      try {
        await client.postChannelMessage(selectedChannel, trimmed);
        if (overridePrompt === undefined) updateDraft("", true);
        setRefreshVersion((version) => version + 1);
      } catch (err) {
        setStatusError(err instanceof Error ? err.message : "Failed to post channel message.");
      } finally {
        setSending(false);
      }
      return;
    }
    await dispatchDirect(trimmed, undefined, overridePrompt === undefined);
  }

  async function handleApprovalDecision(requestId: string, approved: boolean) {
    setStatusError(null);
    try {
      const res = await client.respondApproval({ request_id: requestId, approved });
      const nextStatus =
        res.status || (res.resolved ? (approved ? "approved" : "denied") : "expired");
      setApprovals((prev) => prev.filter((a) => a.request_id !== requestId));
      setMessages((prev) =>
        prev.map((m) => {
          const card = parseMessageCard(m);
          if (card && (card.request_id === requestId || m.request_id === requestId)) {
            return { ...m, card: { ...card, status: nextStatus } };
          }
          return m;
        }),
      );
      setStatusNotice(approved ? "Approved request." : "Denied request.");
      setRefreshVersion((version) => version + 1);
    } catch (err) {
      setStatusError(err instanceof Error ? err.message : "Failed to respond to approval");
    }
  }

  async function handleTriggerRoutine(routineId: string) {
    const deliveryId = `web-${routineId}-${Date.now()}`;
    try {
      const res = await client.triggerRoutine(routineId, deliveryId);
      const status = typeof res.status === "string" ? res.status : "queued";
      setRoutineTriggerResult((prev) => ({ ...prev, [routineId]: status }));
      setRefreshVersion((v) => v + 1);
    } catch (err) {
      setRoutineTriggerResult((prev) => ({
        ...prev,
        [routineId]: err instanceof Error ? err.message : "error",
      }));
    }
  }

  function handleOpenSplitSpace(slug: string) {
    if (slug === splitSpaceSlug) return;
    setSplitSpacePreview(null);
    setSplitSpaceSlug(slug);
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
      if (Array.isArray(sp)) setSpaces(sp);
      setRefreshVersion((v) => v + 1);
      handleOpenSplitSpace(slug);
    } catch (err) {
      setStatusError(err instanceof Error ? err.message : "Failed to create Page");
    }
  }

  async function handleProbeConnector(connectorId: string) {
    setProbeResults((prev) => ({ ...prev, [connectorId]: "probing…" }));
    try {
      const res = await client.probeIntegration(connectorId);
      setProbeResults((prev) => ({ ...prev, [connectorId]: JSON.stringify(res, null, 2) }));
    } catch (err) {
      setStatusError(String(err));
      setProbeResults((prev) => ({ ...prev, [connectorId]: "setup_required" }));
    }
  }

  async function handleSignIn(e: React.FormEvent) {
    e.preventDefault();
    const token = credentialInput.trim();
    if (!connectionMode || !token || signInBusy) return;
    setStatusError(null);
    setSignInBusy(true);
    setCredentialInput("");
    try {
      const session = connectionMode === "local"
        ? { ws_id: await client.connectLocal(token), user_id: "local" }
        : await client.redeemEnrollment(token);
      if (sessionUser && (session.ws_id !== workspaceId || session.user_id !== sessionUser)) {
        clearWorkspaceState();
      }
      setWorkspaceId(session.ws_id);
      setSessionUser(session.user_id);
      setView("chats");
      setRefreshVersion((v) => v + 1);
      setStatusNotice(`Connected to workspace ${session.ws_id}.`);
    } catch (err) {
      setStatusError(err instanceof Error ? err.message : "Connection failed");
    } finally {
      setSignInBusy(false);
    }
  }

  const activeBot = useMemo<BotSummary>(
    () =>
      bots.find((b) => b.id === selectedBot) ||
      DEFAULT_BOTS.find((b) => b.id === selectedBot) || {
        id: selectedBot,
        display_name: selectedBot,
      },
    [bots, selectedBot],
  );

  const activeChannel = useMemo<Channel>(
    () =>
      channels.find((c) => c.id === selectedChannel) || {
        id: selectedChannel,
        name: selectedChannel,
      },
    [channels, selectedChannel],
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
        label: `#${ch.name || ch.id} · ${ch.topic || "Multi-bot channel"}`,
        category: "Channels",
        run: () => selectConversation("channel", ch.id),
      });
    }
    for (const tab of NAV_TABS) {
      items.push({
        id: `nav-${tab.id}`,
        label: `Go to ${tab.label}`,
        category: "Navigation",
        run: () => setView(tab.id),
      });
    }
    items.push({
      id: "act-theme",
      label: `Switch to ${theme === "dark" ? "Light" : "Dark"} Theme`,
      category: "Appearance",
      run: () => setTheme((t) => (t === "dark" ? "light" : "dark")),
    });
    const q = commandQuery.trim().toLowerCase();
    return q
      ? items.filter((i) => i.label.toLowerCase().includes(q) || i.category.toLowerCase().includes(q))
      : items;
  }, [bots, channels, commandQuery, selectConversation, theme]);

  const doctorChecks = useMemo(() => {
    if (!doctor || !Array.isArray(doctor.checks)) return [];
    return doctor.checks as Array<{ id: string; category: string; outcome: string; summary: string }>;
  }, [doctor]);

  const doctorCapabilities = useMemo(() => {
    if (!doctor || !Array.isArray(doctor.capability_readiness)) return [];
    return doctor.capability_readiness as Array<{ id: string; name: string; readiness: string; summary: string }>;
  }, [doctor]);

  async function handleSignOut() {
    setSignInBusy(true);
    setCredentialInput("");
    clearWorkspaceState();
    setSessionUser(null);
    try {
      await client.signOut();
    } catch {
      setStatusError("DT could not confirm hosted sign-out.");
    } finally {
      setSignInBusy(false);
    }
  }

  async function loadRoutineRuns(routineId: string) {
    setSelectedRoutineId(routineId);
    setRoutineRuns([]);
    setStatusError(null);
    try {
      const runs = await client.listRoutineRuns(routineId);
      setRoutineRuns(Array.isArray(runs) ? runs : []);
    } catch (err) {
      setStatusError(err instanceof Error ? err.message : "Failed to load routine runs");
    }
  }

  if (!sessionChecked) {
    return (
      <div className="dt-signin-shell">
        <div className="dt-signin-card">
          <strong>DT</strong>
          {statusError ? (
            <>
              <p role="alert">{statusError}</p>
              <button
                type="button"
                onClick={() => {
                  setStatusError(null);
                  setConnectionAttempt((attempt) => attempt + 1);
                }}
              >
                Retry connection
              </button>
            </>
          ) : (
            <p className="dt-muted">Checking workspace session…</p>
          )}
        </div>
      </div>
    );
  }

  if (!sessionUser) {
    const credentialLabel = connectionMode === "local" ? "Companion token" : "Operator enrollment token";
    return (
      <div className="dt-signin-shell">
        <div className="dt-signin-card">
          <div className="dt-row" style={{ justifyContent: "space-between" }}>
            <h1 style={{ margin: 0, fontSize: 22 }}>Connect to DT</h1>
            <button type="button" onClick={() => setTheme(theme === "dark" ? "light" : "dark")}>
              {theme === "dark" ? "Light theme" : "Dark theme"}
            </button>
          </div>
          <p className="dt-muted" style={{ margin: 0 }}>
            {connectionMode === "local"
              ? "Enter the Companion token from your DT storage directory."
              : "Enter your operator enrollment token to connect to your workspace."}
          </p>
          {statusError ? (
            <div role="alert" className="dt-card dt-alert-error" style={{ marginBottom: 0 }}>
              {statusError}
            </div>
          ) : null}
          <form onSubmit={(e) => void handleSignIn(e)} style={{ display: "flex", flexDirection: "column", gap: 10 }}>
            <input
              value={credentialInput}
              onChange={(e) => setCredentialInput(e.target.value)}
              type="password"
              autoComplete="off"
              aria-label={credentialLabel}
              placeholder={`Paste ${credentialLabel.toLowerCase()}…`}
            />
            <button
              type="submit"
              className="dt-btn-primary"
              disabled={signInBusy || !credentialInput.trim()}
            >
              {signInBusy ? "Connecting…" : "Connect Workspace"}
            </button>
          </form>
        </div>
      </div>
    );
  }

  return (
    <div className="dt-shell" style={{ height: "100dvh", overflow: "hidden" }}>
      <div
        className={`dt-mobile-backdrop dt-drawer-backdrop ${mobileMenuOpen ? "dt-drawer-backdrop-open" : ""}`}
        data-mobile-open={mobileMenuOpen ? "true" : "false"}
        onClick={() => setMobileMenuOpen(false)}
      />

      <aside
        className={`dt-sidebar ${mobileMenuOpen ? "dt-sidebar-open" : ""}`}
        data-mobile-open={mobileMenuOpen ? "true" : "false"}
        style={{
          width: sidebarCollapsed ? 72 : sidebarWidth,
          minWidth: sidebarCollapsed ? 72 : 240,
          maxWidth: 480,
        }}
      >
        <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", padding: "0 4px", gap: 6 }}>
          {!sidebarCollapsed ? (
            <div style={{ display: "flex", alignItems: "center", gap: 8, minWidth: 0 }}>
              <span className="dt-working-dot" title="Runtime active" />
              <strong style={{ fontSize: 15, letterSpacing: "-0.01em", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                DT · {workspaceId}
              </strong>
            </div>
          ) : (
            <strong style={{ fontSize: 14, margin: "0 auto" }}>DT</strong>
          )}
          <button
            type="button"
            className="dt-desktop-collapse"
            aria-label="Toggle sidebar"
            title="Toggle sidebar (Ctrl+B)"
            onClick={() => setSidebarCollapsed((c) => !c)}
            style={{
              width: 30,
              height: 30,
              padding: 0,
              borderRadius: 8,
              border: "1px solid var(--border)",
              background: "var(--card)",
              display: "inline-grid",
              placeItems: "center",
              flexShrink: 0,
            }}
          >
            {sidebarCollapsed ? "→" : "←"}
          </button>
          <button
            type="button"
            className="dt-mobile-only"
            aria-label="Close navigation"
            onClick={() => setMobileMenuOpen(false)}
            style={{
              width: 30,
              height: 30,
              padding: 0,
              alignItems: "center",
              justifyContent: "center",
              borderRadius: 8,
              border: "1px solid var(--border)",
              background: "var(--card)",
              flexShrink: 0,
            }}
          >
            ✕
          </button>
        </div>

        <nav
          aria-label="Sections"
          style={{
            display: "grid",
            gridTemplateColumns: sidebarCollapsed ? "1fr" : "repeat(3, 1fr)",
            gap: 4,
          }}
        >
          {NAV_TABS.map((tab) => {
            const active = view === tab.id;
            return (
              <button
                key={tab.id}
                type="button"
                className={active ? "dt-btn-active" : ""}
                aria-current={active ? "page" : undefined}
                onClick={() => {
                  setView(tab.id);
                  setMobileMenuOpen(false);
                }}
                style={{
                  padding: sidebarCollapsed ? "8px 4px" : "7px 6px",
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
            const isSelected = view === "chats" && chatKind === "dm" && selectedBot === b.id;
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
            const isSelected = view === "chats" && chatKind === "channel" && selectedChannel === ch.id;
            return (
              <button
                key={ch.id}
                type="button"
                className={`dt-chat-item ${isSelected ? "active" : ""}`}
                onClick={() => selectConversation("channel", ch.id)}
                title={`#${ch.name || ch.id}`}
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
                    flexShrink: 0,
                  }}
                >
                  #
                </span>
                {!sidebarCollapsed && (
                  <div style={{ minWidth: 0, flex: 1 }}>
                    <strong style={{ fontSize: 13, fontWeight: 520, display: "block", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                      {ch.name || ch.id}
                    </strong>
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

          {!sidebarCollapsed && (
            <form
              onSubmit={(e) => {
                e.preventDefault();
                if (!newChannelId.trim()) return;
                void client
                  .createChannel(newChannelId.trim(), newChannelTopic.trim())
                  .then(() => {
                    setNewChannelId("");
                    setNewChannelTopic("");
                    setRefreshVersion((v) => v + 1);
                  })
                  .catch((err) => setStatusError(String(err)));
              }}
              style={{ display: "flex", flexDirection: "column", gap: 6, marginTop: 8, padding: "0 4px" }}
            >
              <input
                value={newChannelId}
                onChange={(e) => setNewChannelId(e.target.value)}
                aria-label="New group ID"
                placeholder="New group slug…"
              />
              <button type="submit" disabled={!newChannelId.trim()}>
                + New Group
              </button>
            </form>
          )}
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

      <div style={{ flex: 1, display: "flex", minWidth: 0, height: "100dvh", overflow: "hidden" }}>
        <main className="dt-main" style={{ flex: 1, display: "flex", flexDirection: "column", minWidth: 0, overflow: "hidden" }}>
          <header className="dt-header" style={{ flexShrink: 0 }}>
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
                  alignItems: "center",
                  justifyContent: "center",
                }}
              >
                ☰
              </button>
              <div className="dt-title-pill">
                {view === "chats" ? (
                  chatKind === "dm" ? (
                    <>
                      <BotAvatar bot={activeBot} size={24} />
                      <strong style={{ fontSize: 13.5 }}>{activeBot.display_name || activeBot.id}</strong>
                      <span style={{ fontSize: 12, color: "var(--muted-foreground)" }}>
                        · {activeBot.role_title || "Coworker"}
                      </span>
                    </>
                  ) : (
                    <>
                      <strong style={{ fontSize: 13.5 }}>#{activeChannel.name || activeChannel.id}</strong>
                      <span style={{ fontSize: 12, color: "var(--muted-foreground)" }}>
                        · {activeChannel.topic || (activeChannel.participants || ["muse"]).map((p) => `@${p}`).join(" ")}
                      </span>
                    </>
                  )
                ) : (
                  <>
                    <strong style={{ fontSize: 13.5 }}>
                      {NAV_TABS.find((t) => t.id === view)?.label || view}
                    </strong>
                    <span className="dt-badge">{workspaceId}</span>
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
              <button
                type="button"
                onClick={() => void handleSignOut()}
                disabled={signInBusy}
                style={{
                  padding: "5px 11px",
                  borderRadius: 999,
                  border: "1px solid var(--border)",
                  background: "var(--card)",
                  fontSize: 12,
                }}
              >
                Sign out
              </button>
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
                flexShrink: 0,
              }}
            >
              <span>{statusError}</span>
              <div style={{ display: "flex", alignItems: "center", gap: 6 }}>
                <button
                  type="button"
                  onClick={() => setRefreshVersion((v) => v + 1)}
                  style={{ padding: "4px 10px", fontSize: 12 }}
                >
                  Retry
                </button>
                <button
                  type="button"
                  onClick={() => setStatusError(null)}
                  aria-label="Dismiss error"
                  style={{ border: 0, background: "transparent", color: "var(--muted-foreground)", padding: "4px 6px" }}
                >
                  ✕
                </button>
              </div>
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
                flexShrink: 0,
              }}
            >
              <span>{statusNotice}</span>
              <button
                type="button"
                onClick={() => setStatusNotice(null)}
                aria-label="Dismiss notice"
                style={{ border: 0, background: "transparent", color: "var(--muted-foreground)", padding: "4px 6px" }}
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
                <section style={{ display: "flex", flexDirection: "column", gap: 16 }}>
                  <div>
                    <h2 style={{ margin: 0, fontSize: 20 }}>Providers & Models</h2>
                    <p style={{ margin: "4px 0 0", color: "var(--muted-foreground)", fontSize: 13 }}>
                      5-tier precedence: <code>Routine → Bot → Workspace → Account → Legacy</code> (fails in place without silent model substitution).
                    </p>
                  </div>

                  {providers.length === 0 ? (
                    <div className="dt-card">
                      No providers configured yet. Configure credentials below or via <code>dt onboard</code>.
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
                          <article
                            key={p.id}
                            className="dt-card"
                            style={{ display: "flex", flexDirection: "column", gap: 10, marginBottom: 0 }}
                          >
                            <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", gap: 8 }}>
                              <div>
                                <strong style={{ fontSize: 15 }}>{p.display_name}</strong>
                                <span style={{ marginLeft: 8, fontSize: 12, color: "var(--muted-foreground)" }}>
                                  ({p.id}{p.selected_model ? ` · ${p.selected_model}` : ""})
                                </span>
                              </div>
                              <span
                                className="dt-badge"
                                style={{
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
                            {p.last_error ? (
                              <p style={{ margin: 0, color: "var(--warning)", fontSize: 13 }}>{p.last_error}</p>
                            ) : null}
                            <div className="dt-row" style={{ marginTop: 4 }}>
                              <input
                                type="password"
                                autoComplete="off"
                                aria-label={`API key for ${p.display_name}`}
                                placeholder="API key (optional replacement)"
                                value={providerKeyDrafts[p.id] ?? ""}
                                onChange={(e) =>
                                  setProviderKeyDrafts((prev) => ({ ...prev, [p.id]: e.target.value }))
                                }
                              />
                              <button
                                type="button"
                                className="dt-btn-primary"
                                onClick={() => {
                                  const apiKey = (providerKeyDrafts[p.id] ?? "").trim();
                                  setProviderKeyDrafts((prev) => ({ ...prev, [p.id]: "" }));
                                  void client
                                    .providerAction(p.id, "configure", {
                                      profile: p,
                                      api_key: apiKey || null,
                                    })
                                    .then(() => {
                                      setStatusNotice(`Updated ${p.display_name}`);
                                      setRefreshVersion((v) => v + 1);
                                    })
                                    .catch((err) => setStatusError(String(err)));
                                }}
                              >
                                Save & Connect
                              </button>
                              <button
                                type="button"
                                onClick={() =>
                                  void client
                                    .providerAction(p.id, "refresh")
                                    .then(() => setRefreshVersion((v) => v + 1))
                                    .catch((err) => setStatusError(String(err)))
                                }
                              >
                                Refresh Models
                              </button>
                            </div>
                          </article>
                        );
                      })}
                    </div>
                  )}
                </section>
              ) : view === "routines" ? (
                <section style={{ display: "flex", flexDirection: "column", gap: 16 }}>
                  <div>
                    <h2 style={{ margin: 0, fontSize: 20 }}>Scheduled Routines & Proactive Patrols</h2>
                    <p style={{ margin: "4px 0 0", color: "var(--muted-foreground)", fontSize: 13 }}>
                      IANA timezone-aware schedules with quiet-hours suppression and idempotent delivery deduplication.
                    </p>
                  </div>

                  <form
                    className="dt-card"
                    style={{ marginBottom: 0 }}
                    onSubmit={(e) => {
                      e.preventDefault();
                      if (!newRoutineName.trim() || !newRoutineInstruction.trim()) return;
                      void client
                        .saveRoutine({
                          name: newRoutineName.trim(),
                          bot_id: selectedBot,
                          instruction: newRoutineInstruction.trim(),
                          enabled: true,
                        })
                        .then(() => {
                          setNewRoutineName("");
                          setNewRoutineInstruction("");
                          setRefreshVersion((v) => v + 1);
                        })
                        .catch((err) => setStatusError(String(err)));
                    }}
                  >
                    <div className="dt-row">
                      <input
                        value={newRoutineName}
                        onChange={(e) => setNewRoutineName(e.target.value)}
                        placeholder="Routine name (e.g. Weekly review)"
                        aria-label="Routine name"
                      />
                      <input
                        value={newRoutineInstruction}
                        onChange={(e) => setNewRoutineInstruction(e.target.value)}
                        placeholder="Instruction…"
                        aria-label="Routine instruction"
                      />
                      <button
                        type="submit"
                        className="dt-btn-primary"
                        disabled={!newRoutineName.trim() || !newRoutineInstruction.trim()}
                      >
                        + Create Routine
                      </button>
                    </div>
                  </form>

                  {routines.length === 0 ? (
                    <div className="dt-card">No routines configured.</div>
                  ) : (
                    <div style={{ display: "grid", gap: 10 }}>
                      {routines.map((routine) => (
                        <article key={routine.id} className="dt-card" style={{ marginBottom: 0 }}>
                          <div className="dt-row" style={{ justifyContent: "space-between" }}>
                            <div>
                              <strong>{routine.name}</strong>
                              <span style={{ marginLeft: 8, fontSize: 12, color: "var(--muted-foreground)" }}>
                                @{routine.bot_id || "muse"}
                              </span>
                            </div>
                            <span className="dt-badge">{routine.enabled ? "Enabled" : "Disabled"}</span>
                          </div>
                          {routine.instruction ? (
                            <p style={{ margin: "6px 0 0", fontSize: 13, color: "var(--muted-foreground)" }}>
                              {routine.instruction}
                            </p>
                          ) : null}
                          {routine.next_run_utc ? (
                            <p className="dt-muted" style={{ margin: "4px 0 0" }}>
                              Next run: {routine.next_run_utc}
                            </p>
                          ) : null}
                          <div className="dt-row" style={{ marginTop: 10 }}>
                            <button type="button" onClick={() => void handleTriggerRoutine(routine.id)}>
                              Trigger Now
                            </button>
                            <button type="button" onClick={() => void loadRoutineRuns(routine.id)}>
                              Run history
                            </button>
                            <button
                              type="button"
                              onClick={() =>
                                void client
                                  .testRoutine(routine.id)
                                  .then(() => loadRoutineRuns(routine.id))
                                  .catch((err) => setStatusError(String(err)))
                              }
                            >
                              Test run
                            </button>
                            {routineTriggerResult[routine.id] ? (
                              <span className="dt-badge">{routineTriggerResult[routine.id]}</span>
                            ) : null}
                          </div>
                          {selectedRoutineId === routine.id ? (
                            <div style={{ marginTop: 10 }}>
                              <strong>Recent runs</strong>
                              {routineRuns.length === 0 ? (
                                <p className="dt-muted">No recorded runs yet.</p>
                              ) : (
                                routineRuns.map((run) => (
                                  <p key={run.id} style={{ margin: "4px 0" }}>
                                    {run.status} · {new Date(run.fired_at).toLocaleString()}
                                  </p>
                                ))
                              )}
                            </div>
                          ) : null}
                        </article>
                      ))}
                    </div>
                  )}
                </section>
              ) : view === "activity" ? (
                <section style={{ display: "flex", flexDirection: "column", gap: 16 }}>
                  <h2 style={{ margin: 0, fontSize: 20 }}>Activity & Tasks</h2>
                  <form
                    className="dt-card"
                    style={{ marginBottom: 0 }}
                    onSubmit={(e) => {
                      e.preventDefault();
                      if (!newPostTitle.trim() || !newPostBody.trim()) return;
                      void client
                        .createActivityPost(newPostTitle.trim(), newPostBody.trim())
                        .then(() => {
                          setNewPostTitle("");
                          setNewPostBody("");
                          setRefreshVersion((v) => v + 1);
                        })
                        .catch((err) => setStatusError(String(err)));
                    }}
                  >
                    <div className="dt-row">
                      <input
                        value={newPostTitle}
                        onChange={(e) => setNewPostTitle(e.target.value)}
                        placeholder="Headline…"
                        aria-label="Activity title"
                      />
                      <input
                        value={newPostBody}
                        onChange={(e) => setNewPostBody(e.target.value)}
                        placeholder="Body…"
                        aria-label="Activity body"
                      />
                      <button
                        type="submit"
                        className="dt-btn-primary"
                        disabled={!newPostTitle.trim() || !newPostBody.trim()}
                      >
                        Post
                      </button>
                    </div>
                  </form>

                  {posts.length === 0 ? <p className="dt-muted">No activity updates yet.</p> : null}
                  {posts.map((post) => (
                    <article key={post.id} className="dt-card" style={{ marginBottom: 0 }}>
                      <strong>{post.title}</strong>
                      <p style={{ margin: "6px 0 0" }}>{post.body}</p>
                    </article>
                  ))}

                  <h3 style={{ margin: "8px 0 0", fontSize: 16 }}>Tasks</h3>
                  {tasks.length === 0 ? <p className="dt-muted">No tasks.</p> : null}
                  {tasks.map((task) => (
                    <article key={task.id} className="dt-card" style={{ marginBottom: 0 }}>
                      <p style={{ margin: "0 0 8px" }}>
                        {task.instruction} · <span className="dt-badge">{task.status}</span>
                      </p>
                      <div className="dt-row">
                        {(["pause", "resume", "cancel", "wheel-resume"] as const).map((action) => (
                          <button
                            type="button"
                            key={action}
                            onClick={() => {
                              void client
                                .controlTask(task.id, action)
                                .then(({ ok }) => {
                                  if (!ok) {
                                    setStatusError("Task action is unavailable in its current state.");
                                  }
                                  setRefreshVersion((version) => version + 1);
                                })
                                .catch((error) => setStatusError(String(error)));
                            }}
                          >
                            {action === "wheel-resume" ? "Resume after handoff" : action}
                          </button>
                        ))}
                      </div>
                    </article>
                  ))}
                </section>
              ) : view === "goals" ? (
                <section style={{ display: "flex", flexDirection: "column", gap: 16 }}>
                  <h2 style={{ margin: 0, fontSize: 20 }}>Goals</h2>
                  <form
                    className="dt-card"
                    style={{ marginBottom: 0 }}
                    onSubmit={(e) => {
                      e.preventDefault();
                      if (!newGoalTitle.trim()) return;
                      void client
                        .createGoal(newGoalTitle.trim(), newGoalDesc.trim())
                        .then(() => {
                          setNewGoalTitle("");
                          setNewGoalDesc("");
                          setRefreshVersion((v) => v + 1);
                        })
                        .catch((err) => setStatusError(String(err)));
                    }}
                  >
                    <div className="dt-row">
                      <input
                        value={newGoalTitle}
                        onChange={(e) => setNewGoalTitle(e.target.value)}
                        placeholder="Goal title…"
                        aria-label="Goal title"
                      />
                      <input
                        value={newGoalDesc}
                        onChange={(e) => setNewGoalDesc(e.target.value)}
                        placeholder="Description…"
                        aria-label="Goal description"
                      />
                      <button type="submit" className="dt-btn-primary" disabled={!newGoalTitle.trim()}>
                        + Create Goal
                      </button>
                    </div>
                  </form>
                  {goals.length === 0 ? <p className="dt-muted">No goals yet.</p> : null}
                  {goals.map((g) => (
                    <article key={g.slug} className="dt-card" style={{ marginBottom: 0 }}>
                      <div className="dt-row" style={{ justifyContent: "space-between" }}>
                        <strong>{g.title}</strong>
                        <span className="dt-badge">{g.status || "Active"}</span>
                      </div>
                      {g.description ? <p style={{ margin: "6px 0" }}>{g.description}</p> : null}
                      {g.status !== "Completed" ? (
                        <button
                          type="button"
                          onClick={() =>
                            void client
                              .completeGoal(g.slug)
                              .then(() => setRefreshVersion((v) => v + 1))
                              .catch((err) => setStatusError(String(err)))
                          }
                        >
                          Mark Complete
                        </button>
                      ) : null}
                    </article>
                  ))}
                </section>
              ) : view === "spaces" ? (
                <section style={{ display: "flex", flexDirection: "column", gap: 16 }}>
                  <div>
                    <h2 style={{ margin: 0, fontSize: 20 }}>Pages & Token-Isolated Static Spaces</h2>
                    <p style={{ margin: "4px 0 0", color: "var(--muted-foreground)", fontSize: 13 }}>
                      Served from a separate loopback origin inside a strict <code>sandbox="allow-scripts"</code> frame.
                    </p>
                  </div>

                  <form
                    onSubmit={(e) => void handleCreateSpace(e)}
                    className="dt-card"
                    style={{ display: "flex", flexWrap: "wrap", gap: 8, marginBottom: 0 }}
                  >
                    <input
                      value={newSpaceSlug}
                      onChange={(e) => setNewSpaceSlug(e.target.value)}
                      placeholder="Slug (e.g. sprint-dashboard)"
                      aria-label="Page slug"
                      style={{ flex: "1 1 180px" }}
                    />
                    <input
                      value={newSpaceTitle}
                      onChange={(e) => setNewSpaceTitle(e.target.value)}
                      placeholder="Page title"
                      aria-label="Page title"
                      style={{ flex: "2 1 240px" }}
                    />
                    <button
                      type="submit"
                      className="dt-btn-primary"
                      disabled={!newSpaceSlug.trim()}
                    >
                      New Page
                    </button>
                  </form>

                  {spaces.length === 0 ? (
                    <div className="dt-card">
                      No published Pages yet. Create one above or ask @muse to generate an interactive Space.
                    </div>
                  ) : (
                    <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fill, minmax(250px, 1fr))", gap: 12 }}>
                      {spaces.map((s) => (
                        <article
                          key={s.slug}
                          className="dt-card"
                          style={{
                            display: "flex",
                            flexDirection: "column",
                            justifyContent: "space-between",
                            gap: 12,
                            marginBottom: 0,
                          }}
                        >
                          <div>
                            <strong style={{ fontSize: 15 }}>{s.title || s.slug}</strong>
                            <small style={{ display: "block", color: "var(--muted-foreground)", marginTop: 2 }}>
                              sandbox://{s.slug}
                            </small>
                          </div>
                          <div className="dt-row">
                            <button
                              type="button"
                              onClick={() => handleOpenSplitSpace(s.slug)}
                            >
                              Open Split View
                            </button>
                            <button
                              type="button"
                              onClick={() =>
                                void client
                                  .getSpaceUrl(s.slug)
                                  .then(({ url }) => window.open(url, "_blank", "noopener,noreferrer"))
                                  .catch((err) => setStatusError(String(err)))
                              }
                            >
                              Open in Tab
                            </button>
                          </div>
                        </article>
                      ))}
                    </div>
                  )}
                </section>
              ) : view === "connectors" ? (
                <section style={{ display: "flex", flexDirection: "column", gap: 16 }}>
                  <div>
                    <h2 style={{ margin: 0, fontSize: 20 }}>Connected Apps & Capability Probes</h2>
                    <p style={{ margin: "4px 0 0", color: "var(--muted-foreground)", fontSize: 13 }}>
                      Verify live connector reachability and workspace integration status.
                    </p>
                  </div>
                  {connectors.length === 0 ? (
                    <div className="dt-card">No integrations reported.</div>
                  ) : (
                    <div style={{ display: "grid", gap: 10 }}>
                      {connectors.map((c) => (
                        <article key={c.id} className="dt-card" style={{ marginBottom: 0 }}>
                          <div className="dt-row" style={{ justifyContent: "space-between" }}>
                            <strong>{c.name}</strong>
                            <span className="dt-badge">{c.status}</span>
                          </div>
                          {c.setup_hint ? <p className="dt-muted">{c.setup_hint}</p> : null}
                          <div style={{ marginTop: 8 }}>
                            <button
                              type="button"
                              onClick={() => void handleProbeConnector(c.id)}
                            >
                              Probe capability
                            </button>
                          </div>
                          {probeResults[c.id] ? <pre style={{ marginTop: 8 }}>{probeResults[c.id]}</pre> : null}
                        </article>
                      ))}
                    </div>
                  )}
                </section>
              ) : view === "marketplace" ? (
                <section style={{ display: "flex", flexDirection: "column", gap: 16 }}>
                  <h2 style={{ margin: 0, fontSize: 20 }}>Marketplace</h2>
                  {marketplace.catalog.length === 0 ? (
                    <p className="dt-muted">No marketplace listings available.</p>
                  ) : null}
                  {marketplace.catalog.map((item) => {
                    const isInstalled = marketplace.installed.some((i) => i.id === item.id);
                    return (
                      <article key={item.id} className="dt-card" style={{ marginBottom: 0 }}>
                        <div className="dt-row" style={{ justifyContent: "space-between" }}>
                          <strong>
                            {item.name} · v{item.version}
                          </strong>
                          <span className="dt-badge">{isInstalled ? "Installed" : "Available"}</span>
                        </div>
                        {item.description ? <p className="dt-muted">{item.description}</p> : null}
                        {item.scopes?.length ? (
                          <p className="dt-muted">Scopes: {item.scopes.join(", ")}</p>
                        ) : null}
                        {isInstalled ? (
                          <button
                            type="button"
                            className="dt-btn-danger"
                            onClick={() =>
                              void client
                                .removeMarketplacePlugin(item.id)
                                .then(() => setRefreshVersion((v) => v + 1))
                                .catch((err) => setStatusError(String(err)))
                            }
                          >
                            Remove
                          </button>
                        ) : (
                          <button
                            type="button"
                            className="dt-btn-primary"
                            onClick={() =>
                              void client
                                .installMarketplacePlugin(item.id, item.scopes ?? [])
                                .then(() => setRefreshVersion((v) => v + 1))
                                .catch((err) => setStatusError(String(err)))
                            }
                          >
                            Confirm Scopes & Install
                          </button>
                        )}
                      </article>
                    );
                  })}
                </section>
              ) : view === "billing" ? (
                <section style={{ display: "flex", flexDirection: "column", gap: 16 }}>
                  <h2 style={{ margin: 0, fontSize: 20 }}>Billing & Plan</h2>
                  <div className="dt-card" style={{ marginBottom: 0 }}>
                    <pre style={{ margin: "0 0 12px" }}>{JSON.stringify(billing ?? { status: "unavailable" }, null, 2)}</pre>
                    <button
                      type="button"
                      className="dt-btn-primary"
                      onClick={() =>
                        void client
                          .createBillingCheckout()
                          .then((res) => setStatusNotice(`Checkout: ${JSON.stringify(res)}`))
                          .catch((err) => setStatusError(String(err)))
                      }
                    >
                      Start Hosted Checkout
                    </button>
                  </div>
                </section>
              ) : view === "doctor" ? (
                <section style={{ display: "flex", flexDirection: "column", gap: 16 }}>
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
                    >
                      {showRawDoctor ? "Hide Raw JSON" : "Inspect JSON"}
                    </button>
                  </div>

                  <div
                    className="dt-card"
                    style={{
                      display: "flex",
                      justifyContent: "space-between",
                      alignItems: "center",
                      marginBottom: 0,
                    }}
                  >
                    <div>
                      <small style={{ color: "var(--muted-foreground)", textTransform: "uppercase", fontSize: 11 }}>
                        Installation Readiness
                      </small>
                      <div style={{ fontSize: 18, fontWeight: 650, marginTop: 2 }}>
                        {String(doctor?.installation_readiness ?? "unavailable")}
                      </div>
                    </div>
                    <span className="dt-badge" style={{ color: "var(--success)" }}>
                      Schema v{String(doctor?.schema ?? 1)} · Sanitized
                    </span>
                  </div>

                  {doctorCapabilities.length > 0 && (
                    <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fill, minmax(240px, 1fr))", gap: 10 }}>
                      {doctorCapabilities.map((cap) => (
                        <div key={cap.id} className="dt-card" style={{ marginBottom: 0 }}>
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
                      {doctorChecks.map((chk) => {
                        const outcomeColor =
                          chk.outcome === "pass" || chk.outcome === "repaired"
                            ? "var(--success)"
                            : chk.outcome === "info"
                              ? "var(--link)"
                              : "var(--warning)";
                        return (
                          <div
                            key={chk.id}
                            className="dt-card"
                            style={{
                              display: "flex",
                              justifyContent: "space-between",
                              alignItems: "center",
                              gap: 12,
                              marginBottom: 0,
                            }}
                          >
                            <div>
                              <strong>{chk.id}</strong>
                              <span style={{ marginLeft: 8, fontSize: 12, color: "var(--muted-foreground)" }}>
                                {chk.summary}
                              </span>
                            </div>
                            <span style={{ fontSize: 11.5, fontWeight: 600, color: outcomeColor }}>
                              {chk.outcome}
                            </span>
                          </div>
                        );
                      })}
                    </div>
                  )}

                  {(showRawDoctor || doctorChecks.length === 0) && (
                    <pre className="dt-card" style={{ marginBottom: 0 }}>
                      {JSON.stringify(doctor ?? { status: "unavailable" }, null, 2)}
                    </pre>
                  )}
                </section>
              ) : view === "settings" ? (
                <section style={{ display: "flex", flexDirection: "column", gap: 16 }}>
                  <h2 style={{ margin: 0, fontSize: 20 }}>Settings & Session</h2>
                  <div className="dt-card" style={{ display: "flex", flexDirection: "column", gap: 12, marginBottom: 0 }}>
                    <p className="dt-muted" style={{ margin: 0 }}>
                      Signed in as <strong>{sessionUser}</strong> in workspace <strong>{workspaceId}</strong>.
                    </p>
                    <div className="dt-row">
                      <button type="button" onClick={() => setTheme(theme === "dark" ? "light" : "dark")}>
                        Switch to {theme === "dark" ? "Light" : "Dark"} Theme
                      </button>
                      <button type="button" className="dt-btn-danger" disabled={signInBusy} onClick={() => void handleSignOut()}>
                        Sign out
                      </button>
                    </div>
                    {connectionMode === "hosted" && (
                      <form onSubmit={(e) => void handleSignIn(e)} style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
                        <input
                          value={credentialInput}
                          onChange={(e) => setCredentialInput(e.target.value)}
                          type="password"
                          autoComplete="off"
                          aria-label="Switch workspace enrollment token"
                          placeholder="Operator enrollment token"
                          style={{ flex: "1 1 260px" }}
                        />
                        <button
                          type="submit"
                          className="dt-btn-primary"
                          disabled={signInBusy || !credentialInput.trim()}
                        >
                          {signInBusy ? "Connecting…" : "Connect Workspace"}
                        </button>
                      </form>
                    )}
                  </div>
                </section>
              ) : (
                <section style={{ display: "flex", flexDirection: "column", gap: 14 }}>
                  {approvals.map((a) => (
                    <div key={a.request_id} className="dt-card dt-alert-warn" style={{ marginBottom: 0 }}>
                      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
                        <strong style={{ color: "var(--warning)" }}>⚡ Inline Approval Required · {a.title}</strong>
                        <small style={{ color: "var(--muted-foreground)" }}>Non-blocking</small>
                      </div>
                      <p style={{ margin: "6px 0 10px", fontSize: 13.5 }}>{a.details}</p>
                      <div className="dt-row">
                        <button
                          type="button"
                          className="dt-btn-primary"
                          onClick={() => void handleApprovalDecision(a.request_id, true)}
                        >
                          Approve
                        </button>
                        <button
                          type="button"
                          className="dt-btn-danger"
                          onClick={() => void handleApprovalDecision(a.request_id, false)}
                        >
                          Deny
                        </button>
                      </div>
                    </div>
                  ))}

                  {messages.length === 0 ? (
                    <div
                      className="dt-card"
                      style={{
                        padding: "32px 20px",
                        borderRadius: 20,
                        textAlign: "center",
                        display: "flex",
                        flexDirection: "column",
                        alignItems: "center",
                        gap: 12,
                        marginBottom: 0,
                      }}
                    >
                      {chatKind === "dm" && <BotAvatar bot={activeBot} size={46} />}
                      <div>
                        <strong style={{ fontSize: 16, display: "block" }}>
                          {chatKind === "dm"
                            ? `Start a thread with @${selectedBot}`
                            : `Welcome to #${activeChannel.name || selectedChannel}`}
                        </strong>
                        <p style={{ margin: "4px 0 0", color: "var(--muted-foreground)", fontSize: 13 }}>
                          Conversations are non-blocking: approvals and background tasks stream inline while you keep typing.
                        </p>
                      </div>
                      <div style={{ display: "flex", flexWrap: "wrap", justifyContent: "center", gap: 8, marginTop: 6 }}>
                        {(STARTER_PROMPTS[chatKind === "dm" ? selectedBot : "muse"] || STARTER_PROMPTS.muse).map((item) => (
                          <button
                            key={item.label}
                            type="button"
                            onClick={() => updateDraft(item.prompt)}
                            style={{
                              padding: "7px 13px",
                              borderRadius: 999,
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
                      const author = m.author || (chatKind === "channel" ? selectedChannel : selectedBot);
                      const isUser =
                        m.role === "user" ||
                        author === "You" ||
                        author === "user" ||
                        author === "you";
                      const timestamp = m.created_at || m.timestamp;
                      const card = parseMessageCard(m);
                      const cardKind = card?.kind || card?.type || "";
                      const isApprovalCard = [
                        "approval",
                        "terminal_approval",
                        "ask_first",
                        "order_approval",
                      ].includes(cardKind);
                      const cardStatus = card?.status || "pending";
                      const cardReqId = String(card?.request_id || m.request_id || m.id);
                      return (
                        <div
                          key={m.id}
                          className={`dt-msg ${isUser ? "dt-msg-user" : ""}`}
                          style={{
                            alignSelf: isUser ? "flex-end" : "flex-start",
                            maxWidth: "82%",
                            minWidth: 160,
                            marginBottom: 0,
                          }}
                        >
                          <div style={{ display: "flex", justifyContent: "space-between", gap: 12, marginBottom: 4 }}>
                            <small style={{ fontWeight: 600, fontSize: 11.5, opacity: 0.85 }}>{author}</small>
                            {timestamp && (
                              <small style={{ fontSize: 10.5, opacity: 0.6 }}>{timestamp}</small>
                            )}
                          </div>
                          {m.content ? (
                            <div style={{ whiteSpace: "pre-wrap", overflowWrap: "anywhere", fontSize: 14.5, lineHeight: "21px" }}>
                              {m.content}
                            </div>
                          ) : null}
                          {isApprovalCard && card ? (
                            <div
                              className={`dt-card ${cardStatus === "pending" ? "dt-alert-warn" : ""}`}
                              style={{ marginTop: 8, marginBottom: 0 }}
                            >
                              <div className="dt-row" style={{ justifyContent: "space-between" }}>
                                <strong>{card.title || "Action Approval Required"}</strong>
                                <span className="dt-badge">{cardStatus}</span>
                              </div>
                              {card.details || card.summary ? (
                                <p style={{ margin: "6px 0" }}>{String(card.details || card.summary)}</p>
                              ) : null}
                              {cardStatus === "pending" ? (
                                <div className="dt-row">
                                  <button
                                    type="button"
                                    className="dt-btn-primary"
                                    onClick={() => void handleApprovalDecision(cardReqId, true)}
                                  >
                                    Approve
                                  </button>
                                  <button
                                    type="button"
                                    className="dt-btn-danger"
                                    onClick={() => void handleApprovalDecision(cardReqId, false)}
                                  >
                                    Deny
                                  </button>
                                </div>
                              ) : null}
                            </div>
                          ) : null}
                        </div>
                      );
                    })
                  )}

                  {chatKind === "dm" ? (
                    <div className="dt-card" style={{ marginTop: 4, marginBottom: 0 }}>
                      <div className="dt-row">
                        <button
                          type="button"
                          onClick={() =>
                            void client
                              .listFiles(selectedBot)
                              .then((data) => setFiles(data.files))
                              .catch((error) => setStatusError(String(error)))
                          }
                        >
                          Shared files
                        </button>
                        {files.map(({ path }) => (
                          <button
                            type="button"
                            key={path}
                            onClick={() =>
                              void client
                                .previewFile(selectedBot, path)
                                .then(setFilePreview)
                                .catch((error) => setStatusError(String(error)))
                            }
                          >
                            {path}
                          </button>
                        ))}
                      </div>
                      {filePreview?.preview.kind === "image" && filePreview.data_url ? (
                        <img className="dt-preview" alt="Attachment preview" src={filePreview.data_url} />
                      ) : null}
                      {filePreview?.preview.kind === "pdf" ? (
                        <p>PDF · {filePreview.preview.pdf_pages_estimate} estimated pages</p>
                      ) : null}
                      {filePreview?.preview.kind === "text" ? (
                        <pre>{filePreview.preview.text_snippet ?? filePreview.text}</pre>
                      ) : null}
                    </div>
                  ) : null}
                </section>
              )}
            </div>
          </div>

          {view === "chats" ? (
            <div className="dt-composer-wrap">
              {failedReply ? (
                <div role="alert" className="dt-centered dt-card dt-alert-warn" style={{ marginBottom: 8 }}>
                  <div className="dt-row" style={{ justifyContent: "space-between" }}>
                    <div>
                      <strong>Message saved, reply failed</strong>
                      <div className="dt-muted">{failedReply.detail}</div>
                    </div>
                    <div className="dt-row">
                      <button
                        type="button"
                        className="dt-btn-primary"
                        disabled={sending}
                        onClick={() => void dispatchDirect(failedReply.content, failedReply.clientMsgId, false)}
                      >
                        Retry reply
                      </button>
                      {failedReply.providerSetupRequired ? (
                        <button type="button" onClick={() => setView("providers")}>
                          Open Provider Setup
                        </button>
                      ) : null}
                    </div>
                  </div>
                </div>
              ) : null}
              <form
                className="dt-composer"
                style={{
                  display: "flex",
                  flexDirection: "column",
                  alignItems: "stretch",
                  gap: 6,
                  padding: "10px 14px",
                  borderRadius: "var(--composer-radius, 20px)",
                  maxWidth: 960,
                }}
                onSubmit={(e) => {
                  e.preventDefault();
                  void send();
                }}
              >
                <div style={{ display: "flex", gap: 6, flexWrap: "wrap" }}>
                  {[
                    { label: "/do", insert: "/do " },
                    { label: "@researcher", insert: "@researcher " },
                    { label: "@coder", insert: "@coder " },
                    { label: "→ arrow", insert: `${draft ? `${draft} -> ` : "-> "}` },
                  ].map((chip) => (
                    <button
                      key={chip.label}
                      type="button"
                      disabled={sending}
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
                    disabled={sending}
                    aria-label="Message"
                    placeholder={
                      chatKind === "dm"
                        ? `Message @${selectedBot} (type -> for →, Enter to send)…`
                        : `Post to #${activeChannel.name || selectedChannel} (@mention any bot)…`
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
                    type="submit"
                    className="dt-btn-primary"
                    disabled={sending || !draft.trim()}
                    aria-label="Send"
                    style={{
                      padding: "8px 16px",
                      borderRadius: 999,
                      fontWeight: 600,
                      fontSize: 13,
                    }}
                  >
                    {sending ? "Sending…" : "Send"}
                  </button>
                </div>
              </form>
            </div>
          ) : null}
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
                  setSplitSpacePreview(null);
                }}
              >
                Close
              </button>
            </header>
            {splitSpacePreview && "url" in splitSpacePreview ? (
              <iframe
                title={`Isolated Space ${splitSpaceSlug}`}
                src={splitSpacePreview.url}
                sandbox="allow-scripts"
                style={{ flex: 1, width: "100%", border: 0, background: "#fff" }}
              />
            ) : (
              <div style={{ padding: 20, color: "var(--muted-foreground)" }}>
                {splitSpacePreview ? <p role="alert">{splitSpacePreview.error}</p> : "Loading isolated Page…"}
              </div>
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
