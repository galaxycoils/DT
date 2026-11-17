import React, { useEffect, useState } from "react";
import { MuseV1Client } from "./api";

interface PendingApproval {
  request_id: string;
  title: string;
  details: string;
}

export function App({ client }: { client: MuseV1Client }) {
  const [view, setView] = useState<"chats" | "activity" | "spaces" | "providers">("chats");
  const [bots, setBots] = useState<Array<{ id: string; display_name?: string }>>([]);
  const [selectedBot, setSelectedBot] = useState("muse");
  const [draft, setDraft] = useState("");
  const [messages, setMessages] = useState<Array<{ id: string; sender?: string; content: string }>>([]);
  const [approvals, setApprovals] = useState<PendingApproval[]>([]);
  const [providers, setProviders] = useState<Array<{ id: string; display_name: string; connection_state: string }>>([]);

  useEffect(() => {
    void client.bootstrap().then((snap) => {
      if (Array.isArray(snap.bots)) {
        setBots(snap.bots as Array<{ id: string; display_name?: string }>);
      }
      if (Array.isArray(snap.dm_messages)) {
        setMessages(snap.dm_messages as Array<{ id: string; sender?: string; content: string }>);
      }
      if (Array.isArray(snap.pending_approvals)) {
        setApprovals(snap.pending_approvals as PendingApproval[]);
      }
    });
    void client.getProviders().then((p) => {
      if (Array.isArray(p.profiles)) {
        setProviders(p.profiles as Array<{ id: string; display_name: string; connection_state: string }>);
      }
    });
  }, [client]);

  async function send() {
    if (!draft.trim()) return;
    await client.postDirectMessage(selectedBot, draft.trim());
    setDraft("");
  }

  return (
    <div style={{ display: "flex", height: "100dvh", background: "var(--background)", color: "var(--foreground)" }}>
      <aside style={{ width: 240, borderRight: "1px solid var(--sidebar-border)", background: "var(--sidebar)", padding: 12 }}>
        <div style={{ display: "flex", gap: 6, marginBottom: 12 }}>
          <button type="button" onClick={() => setView("chats")}>Chats</button>
          <button type="button" onClick={() => setView("activity")}>Activity</button>
          <button type="button" onClick={() => setView("spaces")}>Spaces</button>
          <button type="button" onClick={() => setView("providers")}>Providers</button>
        </div>
        {view !== "activity" &&
          bots.map((b) => (
            <button
              key={b.id}
              type="button"
              onClick={() => setSelectedBot(b.id)}
              style={{ display: "block", width: "100%", textAlign: "left", padding: "8px", marginBottom: 4 }}
            >
              {b.display_name || b.id}
            </button>
          ))}
      </aside>
      <main style={{ flex: 1, display: "flex", flexDirection: "column", padding: 16 }}>
        {view === "providers" ? (
          <div>
            <h2>Providers & OAuth</h2>
            {providers.map((p) => (
              <div key={p.id} style={{ padding: 12, marginBottom: 8, background: "var(--card)", borderRadius: 8 }}>
                <strong>{p.display_name}</strong> · <span>{p.connection_state}</span>
              </div>
            ))}
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
              {messages.map((m) => (
                <div key={m.id} style={{ padding: 10, marginBottom: 6, background: "var(--card)", borderRadius: 8 }}>
                  <small>{m.sender || selectedBot}</small>
                  <div>{m.content}</div>
                </div>
              ))}
            </div>
            <div style={{ display: "flex", gap: 8, paddingTop: 10 }}>
              <input
                value={draft}
                onChange={(e) => setDraft(e.target.value)}
                placeholder={`Message @${selectedBot} (non-blocking)...`}
                style={{ flex: 1, padding: 10, background: "var(--card)", color: "var(--foreground)", border: "1px solid var(--border)" }}
              />
              <button type="button" onClick={() => void send()}>Send</button>
            </div>
          </>
        )}
      </main>
    </div>
  );
}
