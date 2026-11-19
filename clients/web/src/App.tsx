import React, { useCallback, useEffect, useRef, useState } from "react";
import {
  createClientMessageId,
  DtV1Client,
  parseMessageCard,
  type ActivityPost,
  type Channel,
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
}

interface FailedReplyBanner {
  clientMsgId: string;
  content: string;
  detail: string;
  providerSetupRequired: boolean;
}

type ViewTab =
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

const DRAFT_STORAGE_KEY = "dt-web-drafts-v1-scoped";

const NAV_TABS: Array<{ id: ViewTab; label: string }> = [
  { id: "chats", label: "Chats" },
  { id: "activity", label: "Activity" },
  { id: "goals", label: "Goals" },
  { id: "routines", label: "Routines" },
  { id: "spaces", label: "Pages" },
  { id: "providers", label: "Providers" },
  { id: "connectors", label: "Apps" },
  { id: "marketplace", label: "Marketplace" },
  { id: "billing", label: "Billing" },
  { id: "doctor", label: "Doctor" },
  { id: "settings", label: "Settings" },
];

export function App({ client }: { client: DtV1Client }) {
  const [view, setView] = useState<ViewTab>("chats");
  const [theme, setTheme] = useState<"dark" | "light">("dark");
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false);
  const [sidebarWidth, setSidebarWidth] = useState(316);
  const [mobileNavOpen, setMobileNavOpen] = useState(false);

  const [sessionUser, setSessionUser] = useState<string | null>(null);
  const [sessionChecked, setSessionChecked] = useState(false);
  const [workspaceId, setWorkspaceId] = useState(client.getWorkspaceId());
  const [enrollmentInput, setEnrollmentInput] = useState("");
  const [signInBusy, setSignInBusy] = useState(false);

  const [bots, setBots] = useState<Array<{ id: string; display_name?: string }>>([]);
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
  const [newRoutineName, setNewRoutineName] = useState("");
  const [newRoutineInstruction, setNewRoutineInstruction] = useState("");

  const [spaces, setSpaces] = useState<Array<{ slug: string; title?: string; published?: boolean }>>([]);
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

  const [refreshVersion, setRefreshVersion] = useState(0);
  const [statusError, setStatusError] = useState<string | null>(null);
  const [statusNotice, setStatusNotice] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  const sessionEpochRef = useRef(0);
  const eventSourceRef = useRef<EventSource | null>(null);

  useEffect(() => {
    document.documentElement.setAttribute("data-theme", theme);
  }, [theme]);

  const activeTarget = chatKind === "channel" ? `channel:${selectedChannel}` : `dm:${selectedBot}`;
  const draftKey = JSON.stringify([sessionUser, workspaceId, activeTarget]);
  const draft = drafts[draftKey] ?? "";

  const clearWorkspaceState = useCallback(() => {
    sessionEpochRef.current += 1;
    eventSourceRef.current?.close();
    eventSourceRef.current = null;
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
    setSpaces([]);
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
    setMobileNavOpen(false);
  }, [client]);

  useEffect(() => {
    try {
      const raw = window.localStorage.getItem(DRAFT_STORAGE_KEY);
      if (raw) setDrafts(JSON.parse(raw) as Record<string, string>);
    } catch {
      // Storage may be unavailable.
    }
    const controller = new AbortController();
    void client
      .restoreSession(controller.signal)
      .then((session) => {
        if (controller.signal.aborted) return;
        setWorkspaceId(session.ws_id);
        setSessionUser(session.user_id);
      })
      .catch(() => {
        if (!controller.signal.aborted) {
          setSessionUser(null);
        }
      })
      .finally(() => {
        if (!controller.signal.aborted) setSessionChecked(true);
      });
    return () => controller.abort();
  }, [client]);

  function updateDraft(value: string) {
    const next = { ...drafts, [draftKey]: value };
    setDrafts(next);
    if (!value.trim()) setPendingClientMsgId(null);
    try {
      window.localStorage.setItem(DRAFT_STORAGE_KEY, JSON.stringify(next));
    } catch {
      // Storage may be unavailable.
    }
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
          setBots(snap.bots as Array<{ id: string; display_name?: string }>);
        }
        if (Array.isArray(snap.pending_approvals)) {
          setApprovals(snap.pending_approvals as PendingApproval[]);
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
    const seenSequences = new Set<string>();
    const invalidated = (ev?: Event) => {
      const msg = ev as MessageEvent | undefined;
      if (msg?.lastEventId) {
        if (seenSequences.has(msg.lastEventId)) return;
        seenSequences.add(msg.lastEventId);
      }
      setRefreshVersion((version) => version + 1);
    };
    const events = new EventSource(client.eventsUrl(), { withCredentials: true });
    eventSourceRef.current = events;
    for (const name of [
      "DIRECT_MESSAGE_ADDED",
      "CARD_UPDATED",
      "STATE_INVALIDATED",
      "TASK_UPDATED",
      "ROUTINE_UPDATED",
      "ROUTINE_QUEUED",
      "BOT_UPDATED",
      "CHANNEL_UPDATED",
      "RESYNC_REQUIRED",
    ]) {
      events.addEventListener(name, invalidated);
    }
    events.addEventListener("open", () => invalidated());
    return () => {
      events.close();
      if (eventSourceRef.current === events) eventSourceRef.current = null;
    };
  }, [client, sessionChecked, sessionUser, workspaceId]);

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
            setConnectors(Array.isArray(data?.integrations) ? data.integrations : []);
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

  async function dispatchDirect(content: string, existingClientMsgId?: string) {
    const clientMsgId = existingClientMsgId || pendingClientMsgId || createClientMessageId();
    setPendingClientMsgId(clientMsgId);
    setStatusError(null);
    setSending(true);
    try {
      const outcome = await client.postDirectMessage(selectedBot, content, clientMsgId);
      updateDraft("");
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

  async function send() {
    const trimmed = draft.trim();
    if (!trimmed || sending) return;
    if (chatKind === "channel") {
      setStatusError(null);
      setSending(true);
      try {
        await client.postChannelMessage(selectedChannel, trimmed);
        updateDraft("");
        setRefreshVersion((version) => version + 1);
      } catch (err) {
        setStatusError(err instanceof Error ? err.message : "Failed to post channel message.");
      } finally {
        setSending(false);
      }
      return;
    }
    await dispatchDirect(trimmed);
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
      setRefreshVersion((version) => version + 1);
    } catch (err) {
      setStatusError(err instanceof Error ? err.message : "Failed to respond to approval");
    }
  }

  async function handleEnrollSignIn(e: React.FormEvent) {
    e.preventDefault();
    if (!enrollmentInput.trim() || signInBusy) return;
    setStatusError(null);
    setSignInBusy(true);
    try {
      const sess = await client.redeemEnrollment(enrollmentInput.trim());
      setEnrollmentInput("");
      setWorkspaceId(sess.ws_id);
      setSessionUser(sess.user_id);
      setView("chats");
    } catch (err) {
      setStatusError(err instanceof Error ? err.message : "Enrollment token rejected");
    } finally {
      setSignInBusy(false);
    }
  }

  async function handleSignOut() {
    clearWorkspaceState();
    setSessionUser(null);
    try {
      await client.signOut();
    } catch {
      // Session cookie is cleared locally even if network fails.
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
          <p className="dt-muted">Checking workspace session…</p>
        </div>
      </div>
    );
  }

  if (!sessionUser) {
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
            Enter your operator enrollment token to start a secure, workspace-scoped session.
          </p>
          {statusError ? (
            <div role="alert" className="dt-card dt-alert-error" style={{ marginBottom: 0 }}>
              {statusError}
            </div>
          ) : null}
          <form onSubmit={(e) => void handleEnrollSignIn(e)} style={{ display: "flex", flexDirection: "column", gap: 10 }}>
            <input
              value={enrollmentInput}
              onChange={(e) => setEnrollmentInput(e.target.value)}
              type="password"
              autoComplete="off"
              aria-label="Operator enrollment token"
              placeholder="Paste operator enrollment token…"
            />
            <button
              type="submit"
              className="dt-btn-primary"
              disabled={signInBusy || !enrollmentInput.trim()}
            >
              {signInBusy ? "Connecting…" : "Connect Workspace"}
            </button>
          </form>
        </div>
      </div>
    );
  }

  return (
    <div className="dt-shell">
      <div
        className={`dt-drawer-backdrop ${mobileNavOpen ? "dt-drawer-backdrop-open" : ""}`}
        onClick={() => setMobileNavOpen(false)}
      />
      <aside
        className={`dt-sidebar ${mobileNavOpen ? "dt-sidebar-open" : ""}`}
        style={{
          width: sidebarCollapsed ? 72 : sidebarWidth,
          minWidth: sidebarCollapsed ? 72 : 240,
          maxWidth: 480,
        }}
      >
        <div className="dt-row" style={{ justifyContent: "space-between" }}>
          <strong>{sidebarCollapsed ? "DT" : `DT · ${workspaceId}`}</strong>
          <button
            type="button"
            className="dt-desktop-collapse"
            aria-label="Toggle sidebar"
            onClick={() => setSidebarCollapsed((c) => !c)}
          >
            {sidebarCollapsed ? "→" : "←"}
          </button>
          <button
            type="button"
            className="dt-mobile-menu-btn"
            aria-label="Close navigation"
            onClick={() => setMobileNavOpen(false)}
          >
            ✕
          </button>
        </div>

        {!sidebarCollapsed ? (
          <input
            className="dt-sidebar-width"
            type="range"
            min={240}
            max={480}
            value={sidebarWidth}
            aria-label="Sidebar width"
            onChange={(event) => setSidebarWidth(event.target.valueAsNumber)}
          />
        ) : null}

        <nav aria-label="Sections" className="dt-row">
          {NAV_TABS.map((tab) => (
            <button
              key={tab.id}
              type="button"
              className={view === tab.id ? "dt-btn-active" : ""}
              aria-current={view === tab.id ? "page" : undefined}
              onClick={() => {
                setView(tab.id);
                setMobileNavOpen(false);
              }}
            >
              {sidebarCollapsed ? tab.label.slice(0, 2) : tab.label}
            </button>
          ))}
        </nav>

        {!sidebarCollapsed ? (
          <>
            <small className="dt-muted">Personal</small>
            {bots.map((b) => {
              const active = view === "chats" && chatKind === "dm" && selectedBot === b.id;
              return (
                <button
                  key={b.id}
                  type="button"
                  className={active ? "dt-btn-active" : ""}
                  onClick={() => {
                    setView("chats");
                    setChatKind("dm");
                    setSelectedBot(b.id);
                    setMessages([]);
                    setFiles([]);
                    setFilePreview(null);
                    setFailedReply(null);
                    setPendingClientMsgId(null);
                    setMobileNavOpen(false);
                  }}
                  style={{ width: "100%", textAlign: "left" }}
                >
                  {b.display_name || b.id}
                </button>
              );
            })}

            {channels.length > 0 ? <small className="dt-muted">Groups</small> : null}
            {channels.map((ch) => {
              const active = view === "chats" && chatKind === "channel" && selectedChannel === ch.id;
              return (
                <button
                  key={ch.id}
                  type="button"
                  className={active ? "dt-btn-active" : ""}
                  onClick={() => {
                    setView("chats");
                    setChatKind("channel");
                    setSelectedChannel(ch.id);
                    setMessages([]);
                    setFailedReply(null);
                    setMobileNavOpen(false);
                  }}
                  style={{ width: "100%", textAlign: "left" }}
                >
                  #{ch.name || ch.id}
                </button>
              );
            })}

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
              style={{ display: "flex", flexDirection: "column", gap: 6, marginTop: 6 }}
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
          </>
        ) : null}
      </aside>

      <main className="dt-main">
        <header className="dt-topbar">
          <div className="dt-row">
            <button
              type="button"
              className="dt-mobile-menu-btn"
              aria-label="Open navigation"
              onClick={() => setMobileNavOpen(true)}
            >
              ☰
            </button>
            <strong>
              {view === "chats"
                ? chatKind === "channel"
                  ? `#${selectedChannel}`
                  : `@${selectedBot}`
                : NAV_TABS.find((t) => t.id === view)?.label || view}
            </strong>
            <span className="dt-badge">{workspaceId}</span>
          </div>
          <div className="dt-row">
            <button type="button" onClick={() => setTheme(theme === "dark" ? "light" : "dark")}>
              {theme === "dark" ? "Light" : "Dark"}
            </button>
            <button type="button" onClick={() => void handleSignOut()}>
              Sign out
            </button>
          </div>
        </header>

        <div className="dt-content">
          <div className="dt-centered">
            {statusError ? (
              <div role="alert" className="dt-card dt-alert-error">
                <div className="dt-row" style={{ justifyContent: "space-between" }}>
                  <span>{statusError}</span>
                  <button type="button" onClick={() => setRefreshVersion((v) => v + 1)}>
                    Retry
                  </button>
                </div>
              </div>
            ) : null}

            {statusNotice ? (
              <div role="status" className="dt-card">
                {statusNotice}
              </div>
            ) : null}

            {loading ? <p className="dt-muted">Loading DT workspace…</p> : null}

            {view === "providers" ? (
              <section>
                <h2>Providers & Models</h2>
                {providers.length === 0 ? (
                  <p className="dt-muted">
                    No providers configured yet. Configure credentials below or via `dt onboard`.
                  </p>
                ) : null}
                {providers.map((p) => (
                  <article key={p.id} className="dt-card">
                    <div className="dt-row" style={{ justifyContent: "space-between" }}>
                      <strong>{p.display_name}</strong>
                      <span className="dt-badge">{p.connection_state}</span>
                    </div>
                    {p.selected_model ? (
                      <p className="dt-muted">Model: {p.selected_model}</p>
                    ) : null}
                    {p.last_error ? (
                      <p style={{ color: "var(--warning)" }}>{p.last_error}</p>
                    ) : null}
                    <div className="dt-row" style={{ marginTop: 8 }}>
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
                ))}
              </section>
            ) : view === "routines" ? (
              <section>
                <h2>Routines</h2>
                <form
                  className="dt-card"
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

                {routines.length === 0 ? <p className="dt-muted">No routines configured.</p> : null}
                {routines.map((routine) => (
                  <article key={routine.id} className="dt-card">
                    <div className="dt-row" style={{ justifyContent: "space-between" }}>
                      <strong>{routine.name}</strong>
                      <span className="dt-badge">{routine.enabled ? "Enabled" : "Disabled"}</span>
                    </div>
                    {routine.next_run_utc ? (
                      <p className="dt-muted">Next run: {routine.next_run_utc}</p>
                    ) : null}
                    <div className="dt-row" style={{ marginTop: 8 }}>
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
              </section>
            ) : view === "activity" ? (
              <section>
                <h2>Activity</h2>
                <form
                  className="dt-card"
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
                  <article key={post.id} className="dt-card">
                    <strong>{post.title}</strong>
                    <p>{post.body}</p>
                  </article>
                ))}

                <h3>Tasks</h3>
                {tasks.length === 0 ? <p className="dt-muted">No tasks.</p> : null}
                {tasks.map((task) => (
                  <article key={task.id} className="dt-card">
                    <p>
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
              <section>
                <h2>Goals</h2>
                <form
                  className="dt-card"
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
                  <article key={g.slug} className="dt-card">
                    <div className="dt-row" style={{ justifyContent: "space-between" }}>
                      <strong>{g.title}</strong>
                      <span className="dt-badge">{g.status || "Active"}</span>
                    </div>
                    {g.description ? <p>{g.description}</p> : null}
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
              <section>
                <h2>Pages & Spaces</h2>
                <form
                  className="dt-card"
                  onSubmit={(e) => {
                    e.preventDefault();
                    if (!newSpaceSlug.trim()) return;
                    void client
                      .createSpace(newSpaceSlug.trim(), newSpaceTitle.trim() || newSpaceSlug.trim())
                      .then(() => {
                        setNewSpaceSlug("");
                        setNewSpaceTitle("");
                        setRefreshVersion((v) => v + 1);
                      })
                      .catch((err) => setStatusError(String(err)));
                  }}
                >
                  <div className="dt-row">
                    <input
                      value={newSpaceSlug}
                      onChange={(e) => setNewSpaceSlug(e.target.value)}
                      placeholder="Page slug…"
                      aria-label="Page slug"
                    />
                    <input
                      value={newSpaceTitle}
                      onChange={(e) => setNewSpaceTitle(e.target.value)}
                      placeholder="Page title…"
                      aria-label="Page title"
                    />
                    <button type="submit" className="dt-btn-primary" disabled={!newSpaceSlug.trim()}>
                      + Create Page
                    </button>
                  </div>
                </form>
                {spaces.length === 0 ? (
                  <p className="dt-muted">No Pages created yet.</p>
                ) : (
                  spaces.map((s) => (
                    <article key={s.slug} className="dt-card dt-row" style={{ justifyContent: "space-between" }}>
                      <strong>{s.title || s.slug}</strong>
                      <button
                        type="button"
                        onClick={() =>
                          void client
                            .getSpaceUrl(s.slug)
                            .then(({ url }) => window.open(url, "_blank", "noopener,noreferrer"))
                            .catch((err) => setStatusError(String(err)))
                        }
                      >
                        Open Page
                      </button>
                    </article>
                  ))
                )}
              </section>
            ) : view === "connectors" ? (
              <section>
                <h2>Connected Apps</h2>
                {connectors.length === 0 ? <p className="dt-muted">No integrations reported.</p> : null}
                {connectors.map((c) => (
                  <article key={c.id} className="dt-card">
                    <div className="dt-row" style={{ justifyContent: "space-between" }}>
                      <strong>{c.name}</strong>
                      <span className="dt-badge">{c.status}</span>
                    </div>
                    {c.setup_hint ? <p className="dt-muted">{c.setup_hint}</p> : null}
                    <button
                      type="button"
                      onClick={() =>
                        void client
                          .probeIntegration(c.id)
                          .then((res) =>
                            setProbeResults((prev) => ({ ...prev, [c.id]: JSON.stringify(res) })),
                          )
                          .catch((err) => setStatusError(String(err)))
                      }
                    >
                      Probe capability
                    </button>
                    {probeResults[c.id] ? <pre>{probeResults[c.id]}</pre> : null}
                  </article>
                ))}
              </section>
            ) : view === "marketplace" ? (
              <section>
                <h2>Marketplace</h2>
                {marketplace.catalog.length === 0 ? (
                  <p className="dt-muted">No marketplace listings available.</p>
                ) : null}
                {marketplace.catalog.map((item) => {
                  const isInstalled = marketplace.installed.some((i) => i.id === item.id);
                  return (
                    <article key={item.id} className="dt-card">
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
              <section>
                <h2>Billing & Plan</h2>
                <pre>{JSON.stringify(billing ?? { status: "unavailable" }, null, 2)}</pre>
                <button
                  type="button"
                  onClick={() =>
                    void client
                      .createBillingCheckout()
                      .then((res) => setStatusNotice(`Checkout: ${JSON.stringify(res)}`))
                      .catch((err) => setStatusError(String(err)))
                  }
                >
                  Start Hosted Checkout
                </button>
              </section>
            ) : view === "doctor" ? (
              <section>
                <h2>DT Doctor Status (Sanitized)</h2>
                <pre>{JSON.stringify(doctor ?? { status: "unavailable" }, null, 2)}</pre>
              </section>
            ) : view === "settings" ? (
              <section>
                <h2>Settings & Hosted Session</h2>
                <p className="dt-muted">
                  Signed in as <strong>{sessionUser}</strong> in workspace <strong>{workspaceId}</strong>.
                </p>
                <div className="dt-row">
                  <button type="button" onClick={() => setTheme(theme === "dark" ? "light" : "dark")}>
                    Switch to {theme === "dark" ? "Light" : "Dark"} Theme
                  </button>
                  <button type="button" className="dt-btn-danger" onClick={() => void handleSignOut()}>
                    Sign out
                  </button>
                </div>
              </section>
            ) : (
              <section>
                {approvals.map((a) => (
                  <div key={a.request_id} className="dt-card dt-alert-warn">
                    <strong>{a.title}</strong>
                    <p>{a.details}</p>
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
                  <p className="dt-muted">
                    Start a thread in {chatKind === "channel" ? `#${selectedChannel}` : `@${selectedBot}`}.
                  </p>
                ) : null}

                {messages.map((m) => {
                  const isUser = m.role === "user" || m.author === "You";
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
                    <div key={m.id} className={`dt-msg ${isUser ? "dt-msg-user" : ""}`}>
                      <small className={isUser ? "" : "dt-muted"}>
                        {m.author || (chatKind === "channel" ? selectedChannel : selectedBot)}
                      </small>
                      {m.content ? <div>{m.content}</div> : null}
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
                            <p>{String(card.details || card.summary)}</p>
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
                })}

                {chatKind === "dm" ? (
                  <div className="dt-card" style={{ marginTop: 12 }}>
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
                      onClick={() => void dispatchDirect(failedReply.content, failedReply.clientMsgId)}
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
              onSubmit={(e) => {
                e.preventDefault();
                void send();
              }}
            >
              <input
                value={draft}
                onChange={(e) => updateDraft(e.target.value)}
                disabled={sending}
                aria-label="Message"
                placeholder={
                  chatKind === "channel"
                    ? `Message #${selectedChannel}…`
                    : `Message @${selectedBot}…`
                }
              />
              <button
                type="submit"
                className="dt-btn-primary"
                disabled={sending || !draft.trim()}
              >
                {sending ? "Sending…" : "Send"}
              </button>
            </form>
          </div>
        ) : null}
      </main>
    </div>
  );
}
