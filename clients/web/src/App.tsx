import React, { useEffect, useState } from "react";
import { DtV1Client, type DirectMessage, type Routine, type Task } from "./api";
import "./layout.css";

interface PendingApproval {
  request_id: string;
  title: string;
  details: string;
}

type ViewTab = "chats" | "activity" | "routines" | "spaces" | "providers" | "doctor" | "settings";

const DRAFT_STORAGE_KEY = "dt-web-drafts-v1";

export function App({ client }: { client: DtV1Client }) {
  const [view, setView] = useState<ViewTab>("chats");
  const [theme, setTheme] = useState<"dark" | "light">("dark");
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false);
  const [sidebarWidth, setSidebarWidth] = useState(316);
  const [bots, setBots] = useState<Array<{ id: string; display_name?: string }>>([]);
  const [selectedBot, setSelectedBot] = useState("muse");
  const [drafts, setDrafts] = useState<Record<string, string>>({});
  const [messages, setMessages] = useState<DirectMessage[]>([]);
  const [routines, setRoutines] = useState<Routine[]>([]);
  const [tasks, setTasks] = useState<Task[]>([]);
  const [posts, setPosts] = useState<Array<{ id: string; title?: string; content?: string }>>([]);
  const [files, setFiles] = useState<Array<{ path: string; size: number }>>([]);
  const [filePreview, setFilePreview] = useState<Awaited<ReturnType<DtV1Client["previewFile"]>> | null>(null);
  const [sessionUser, setSessionUser] = useState<string | null>(null);
  const [sessionChecked, setSessionChecked] = useState(false);
  const [refreshVersion, setRefreshVersion] = useState(0);
  const [sending, setSending] = useState(false);
  const [approvals, setApprovals] = useState<PendingApproval[]>([]);
  const [providers, setProviders] = useState<Array<{ id: string; display_name: string; connection_state: string }>>([]);
  const [spaces, setSpaces] = useState<Array<{ slug: string; title?: string }>>([]);
  const [doctor, setDoctor] = useState<Record<string, unknown> | null>(null);
  const [workspaceId, setWorkspaceId] = useState(client.getWorkspaceId());
  const [enrollmentInput, setEnrollmentInput] = useState("");
  const [statusError, setStatusError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    document.documentElement.setAttribute("data-theme", theme);
  }, [theme]);

  const draftKey = JSON.stringify([sessionUser, workspaceId, selectedBot]);
  const draft = drafts[draftKey] ?? "";

  useEffect(() => {
    try {
      const raw = window.localStorage.getItem(DRAFT_STORAGE_KEY + "-scoped");
      if (raw) setDrafts(JSON.parse(raw) as Record<string, string>);
    } catch {
      // Storage may be unavailable.
    }
    let active = true;
    void client.restoreSession().then((session) => {
      if (!active) return;
      setWorkspaceId(session.ws_id);
      setSessionUser(session.user_id);
    }).catch(() => {}).finally(() => {
      if (active) setSessionChecked(true);
    });
    return () => { active = false; };
  }, [client]);

  function updateDraft(value: string) {
    const next = { ...drafts, [draftKey]: value };
    setDrafts(next);
    try {
      window.localStorage.setItem(DRAFT_STORAGE_KEY + "-scoped", JSON.stringify(next));
    } catch {
      // Storage may be unavailable.
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
      if (Array.isArray(snap.bots)) {
        setBots(snap.bots as Array<{ id: string; display_name?: string }>);
      }
      if (Array.isArray(snap.pending_approvals)) {
        setApprovals(snap.pending_approvals as PendingApproval[]);
      }
      if (Array.isArray(prov.profiles)) {
        setProviders(prov.profiles as Array<{ id: string; display_name: string; connection_state: string }>);
      }
      if (doc) setDoctor(doc);
      const sp = await client.listSpaces().catch(() => []);
      if (Array.isArray(sp)) setSpaces(sp as Array<{ slug: string; title?: string }>);
    } catch (err) {
      setStatusError(err instanceof Error ? err.message : "Failed to load DT workspace");
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    if (sessionChecked) void refreshAll();
  }, [client, sessionChecked, sessionUser, workspaceId]);

  useEffect(() => {
    if (!sessionChecked || !sessionUser) return;
    const invalidated = () => setRefreshVersion((version) => version + 1);
    const events = new EventSource(client.eventsUrl(), { withCredentials: true });
    for (const name of ["DIRECT_MESSAGE_ADDED", "STATE_INVALIDATED", "TASK_UPDATED", "ROUTINE_UPDATED", "ROUTINE_QUEUED"]) {
      events.addEventListener(name, invalidated);
    }
    events.addEventListener("open", invalidated);
    return () => events.close();
  }, [client, sessionChecked, sessionUser, workspaceId]);

  useEffect(() => {
    if (!sessionChecked || !sessionUser) return;
    const controller = new AbortController();
    void client.listDirectMessages(selectedBot, controller.signal).then(setMessages).catch((error) => {
      if (!controller.signal.aborted) setStatusError(String(error));
    });
    if (view === "routines") void client.listRoutines().then(setRoutines).catch((error) => setStatusError(String(error)));
    if (view === "activity") {
      void client.listTasks().then(setTasks).catch((error) => setStatusError(String(error)));
      void client.listActivity().then((data) => setPosts(data.posts)).catch((error) => setStatusError(String(error)));
    }
    return () => controller.abort();
  }, [client, refreshVersion, selectedBot, sessionChecked, sessionUser, view, workspaceId]);

  async function send() {
    if (!draft.trim() || sending) return;
    setStatusError(null);
    setSending(true);
    try {
      await client.postDirectMessage(selectedBot, draft.trim());
      updateDraft("");
      setRefreshVersion((version) => version + 1);
    } catch (err) {
      setStatusError(err instanceof Error ? err.message : "Failed to send message. Draft preserved.");
    } finally {
      setSending(false);
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
      setSessionUser(sess.user_id);
    } catch (err) {
      setStatusError(err instanceof Error ? err.message : "Enrollment token rejected");
    }
  }

  return (
    <div className="dt-shell" style={{ minHeight: "100dvh", background: "var(--background)", color: "var(--foreground)" }}>
      <aside
        className="dt-sidebar"
        style={{
          width: sidebarCollapsed ? 72 : sidebarWidth,
          minWidth: sidebarCollapsed ? 72 : 240,
          maxWidth: 480,
          borderRight: "1px solid var(--sidebar-border)",
          background: "var(--sidebar)",
          padding: 12,
          display: "flex",
          flexDirection: "column",
          gap: 8,
        }}
      >
        <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
          <strong>{sidebarCollapsed ? "DT" : "DT · Workspace"}</strong>
          <button type="button" aria-label="Toggle sidebar" onClick={() => setSidebarCollapsed((c) => !c)}>
            {sidebarCollapsed ? "→" : "←"}
          </button>
        </div>
        {!sidebarCollapsed && <input className="dt-sidebar-width" type="range" min={240} max={480} value={sidebarWidth} aria-label="Sidebar width" onChange={(event) => setSidebarWidth(event.target.valueAsNumber)} />}
        <nav aria-label="Sections" style={{ display: "flex", flexWrap: "wrap", gap: 6 }}>
          {(["chats", "activity", "routines", "spaces", "providers", "doctor", "settings"] as ViewTab[]).map((tab) => (
            <button key={tab} type="button" onClick={() => setView(tab)}>
              {tab[0].toUpperCase() + tab.slice(1)}
            </button>
          ))}
        </nav>
        {!sidebarCollapsed &&
          bots.map((b) => (
            <button
              key={b.id}
              type="button"
              onClick={() => {
                setSelectedBot(b.id);
                setMessages([]);
                setFiles([]);
                setFilePreview(null);
              }}
              style={{ display: "block", width: "100%", textAlign: "left", padding: "8px", background: b.id === selectedBot ? "var(--card)" : "transparent", color: "var(--foreground)", border: "1px solid var(--border)", borderRadius: 8 }}
            >
              {b.display_name || b.id}
            </button>
          ))}
      </aside>
      <main style={{ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", padding: 16, maxWidth: 960, margin: "0 auto", width: "100%" }}>
        {statusError && (
          <div role="alert" style={{ padding: 10, marginBottom: 10, background: "var(--card)", border: "1px solid var(--destructive)", borderRadius: 8 }}>
            {statusError} · Sign in with an operator enrollment token or check daemon connectivity.
          </div>
        )}
        {loading && <div style={{ padding: 8, color: "var(--muted-foreground)" }}>Loading DT workspace…</div>}
        {view === "providers" ? (
          <div>
            <h2>Providers & OAuth</h2>
            {providers.length === 0 ? <p>No providers configured. Add credentials in Settings or `dt onboard`.</p> : null}
            {providers.map((p) => (
              <div key={p.id} style={{ padding: 12, marginBottom: 8, background: "var(--card)", borderRadius: 8 }}>
                <strong>{p.display_name}</strong> · <span>{p.connection_state}</span>
              </div>
            ))}
          </div>
        ) : view === "routines" ? (
          <section>
            <h2>Routines</h2>
            {routines.length === 0 && <p>No routines.</p>}
            {routines.map((routine) => <p key={routine.id}>{routine.name} · {routine.enabled ? "Enabled" : "Disabled"}</p>)}
          </section>
        ) : view === "activity" ? (
          <section>
            <h2>Activity</h2>
            {posts.map((post) => <article key={post.id}><strong>{post.title}</strong><p>{post.content}</p></article>)}
            <h3>Tasks</h3>
            {tasks.length === 0 && <p>No tasks.</p>}
            {tasks.map((task) => (
              <article key={task.id}>
                <p>{task.instruction} · {task.status}</p>
                {(["pause", "resume", "cancel", "wheel-resume"] as const).map((action) => (
                  <button type="button" key={action} onClick={() => {
                    void client.controlTask(task.id, action).then(({ ok }) => {
                      if (!ok) setStatusError("Task action is unavailable in its current state.");
                      setRefreshVersion((version) => version + 1);
                    }).catch((error) => setStatusError(String(error)));
                  }}>{action === "wheel-resume" ? "Resume after handoff" : action}</button>
                ))}
              </article>
            ))}
          </section>
        ) : view === "spaces" ? (
          <div>
            <h2>Pages & Spaces</h2>
            {spaces.length === 0 ? <p>No published Pages yet.</p> : spaces.map((s) => <div key={s.slug}>{s.title || s.slug}</div>)}
          </div>
        ) : view === "doctor" ? (
          <div>
            <h2>DT Doctor Status (Sanitized)</h2>
            <pre style={{ background: "var(--card)", padding: 12, borderRadius: 8, overflow: "auto" }}>
              {JSON.stringify(doctor ?? { status: "unavailable" }, null, 2)}
            </pre>
          </div>
        ) : view === "settings" ? (
          <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
            <h2>Settings & Hosted Session</h2>
            <div style={{ display: "flex", gap: 8 }}>
              <button type="button" onClick={() => setTheme(theme === "dark" ? "light" : "dark")}>
                Switch to {theme === "dark" ? "Light" : "Dark"} Theme
              </button>
              <button type="button" onClick={() => void client.signOut().then(() => {
                setSessionUser(null);
                setBots([]);
                setMessages([]);
                setApprovals([]);
                setFiles([]);
                setFilePreview(null);
                setStatusError(null);
              }).catch((error) => setStatusError(String(error)))}>
                Sign out
              </button>
            </div>
            <form onSubmit={(e) => void handleEnrollSignIn(e)} style={{ display: "flex", gap: 8 }}>
              <input
                value={enrollmentInput}
                onChange={(e) => setEnrollmentInput(e.target.value)}
                type="password"
                autoComplete="off"
                aria-label="Operator enrollment token"
                placeholder="Operator enrollment token"
                style={{ flex: 1, padding: 8, background: "var(--card)", color: "var(--foreground)", border: "1px solid var(--border)" }}
              />
              <button type="submit">Connect Workspace</button>
            </form>
          </div>
        ) : (
          <>
            <div style={{ flex: 1, overflow: "auto" }}>
              {approvals.map((a) => (
                <div key={a.request_id} style={{ padding: 12, marginBottom: 8, background: "var(--card)", border: "1px solid var(--warning)", borderRadius: 8 }}>
                  <strong>{a.title}</strong>
                  <p>{a.details}</p>
                  <button type="button" onClick={() => void client.respondApproval({ request_id: a.request_id, approved: true })}>
                    Approve
                  </button>
                  <button type="button" onClick={() => void client.respondApproval({ request_id: a.request_id, approved: false })}>
                    Deny
                  </button>
                </div>
              ))}
              {messages.length === 0 ? <p style={{ color: "var(--muted-foreground)" }}>Start a thread with @{selectedBot}.</p> : null}
              {messages.map((m) => (
                <div key={m.id} style={{ padding: 10, marginBottom: 6, background: "var(--card)", borderRadius: 8 }}>
                  <small>{m.author || selectedBot}</small>
                  <div>{m.content}</div>
                </div>
              ))}
            </div>
            {sessionUser && <div>
              <button type="button" onClick={() => void client.listFiles(selectedBot).then((data) => setFiles(data.files)).catch((error) => setStatusError(String(error)))}>Shared files</button>
              {files.map(({ path }) => <button type="button" key={path} onClick={() => void client.previewFile(selectedBot, path).then(setFilePreview).catch((error) => setStatusError(String(error)))}>{path}</button>)}
              {filePreview?.preview.kind === "image" && filePreview.data_url && <img className="dt-preview" alt="Attachment preview" src={filePreview.data_url} />}
              {filePreview?.preview.kind === "pdf" && <p>PDF · {filePreview.preview.pdf_pages_estimate} estimated pages</p>}
              {filePreview?.preview.kind === "text" && <pre>{filePreview.preview.text_snippet}</pre>}
            </div>}
            <div style={{ display: "flex", gap: 8, paddingTop: 10 }}>
              <input
                value={draft}
                onChange={(e) => updateDraft(e.target.value)}
                disabled={sending}
                placeholder={`Message @${selectedBot} (draft persisted locally)...`}
                style={{ flex: 1, padding: 10, background: "var(--card)", color: "var(--foreground)", border: "1px solid var(--border)", borderRadius: 26 }}
              />
              <button type="button" disabled={sending || !draft.trim()} onClick={() => void send()}>Send</button>
            </div>
          </>
        )}
      </main>
    </div>
  );
}
