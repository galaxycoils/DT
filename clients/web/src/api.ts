export type ApprovalAction = {
  request_id: string;
  approved: boolean;
  remember_rule?: string;
};

export type MessageCard = {
  kind?: string;
  type?: string;
  request_id?: string;
  title?: string;
  details?: string;
  summary?: string;
  status?: string;
  command?: string;
  [key: string]: unknown;
};

export type DirectMessage = {
  id: string;
  author: string;
  role?: string;
  content: string;
  created_at?: string;
  timestamp?: string;
  request_id?: string | null;
  card?: MessageCard | null;
  card_json?: string | null;
};

export type DirectPostOutcome = {
  user_message?: DirectMessage;
  assistant_message?: DirectMessage | null;
  reply_error?: string | null;
  provider_setup_required?: boolean;
};

export type Routine = {
  id: string;
  name: string;
  enabled: boolean;
  bot_id?: string;
  instruction?: string;
  timezone?: string;
  hour?: number;
  minute?: number;
  next_run_utc?: string | null;
};

export type RoutineRun = {
  id: string;
  fired_at: string;
  status: string;
  task_id?: string | null;
};

export type ActivityPost = {
  id: string;
  title: string;
  body: string;
  category?: string;
  kicker?: string;
  created_at?: string;
};

export type Task = { id: string; instruction: string; status: string; bot_id?: string };
export type Channel = { id: string; name?: string; topic?: string; participants?: string[] };
export type GoalItem = {
  slug: string;
  title: string;
  description?: string;
  status?: string;
  category?: string;
  files?: string[];
};
export type ProviderProfile = {
  id: string;
  display_name: string;
  preset?: string;
  auth_mode?: string;
  protocol?: string;
  base_url?: string;
  selected_model?: string;
  connection_state: string;
  last_error?: string | null;
  discovered_models?: string[];
  model_catalog?: Array<{ id: string; display_name?: string }>;
  health?: { catalog_status?: string; verification_status?: string };
};
export type ConnectorSummary = {
  id: string;
  name: string;
  status: string;
  account_summary?: string;
  setup_hint?: string;
};
export type MarketplaceListing = {
  id: string;
  name: string;
  version: string;
  description?: string;
  scopes?: string[];
};

export interface DtV1ClientOptions {
  baseUrl?: string;
  workspaceId?: string;
  /** Optional memory-only token for native/loopback embed; never persisted in browser storage. */
  memoryToken?: string;
}

export interface HostedSessionInfo {
  authenticated: boolean;
  user_id: string;
  ws_id: string;
  tenant_id: string;
  role: string;
  csrf_token: string;
  expires_at: string;
}

export type ConnectionMode = "local" | "hosted";

export function buildV1Path(suffix: string): string {
  const clean = suffix.startsWith("/") ? suffix : `/${suffix}`;
  return `/api/v1${clean}`;
}

export function workspaceEventsPath(workspaceId: string, afterSeq?: number): string {
  const base = buildV1Path(`/workspaces/${encodeURIComponent(workspaceId)}/events`);
  return typeof afterSeq === "number" && afterSeq > 0 ? `${base}?after=${afterSeq}` : base;
}

export function createClientMessageId(): string {
  const random =
    typeof crypto !== "undefined" && typeof crypto.randomUUID === "function"
      ? crypto.randomUUID()
      : `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
  return `dm-${random}`;
}

export function normalizeRoutine(raw: unknown): Routine {
  const outer = (raw && typeof raw === "object" ? raw : {}) as Record<string, unknown>;
  const inner =
    outer.routine && typeof outer.routine === "object"
      ? (outer.routine as Record<string, unknown>)
      : outer;
  const id = String(inner.id ?? outer.id ?? "");
  const name = String(inner.name ?? outer.name ?? (id || "Unnamed routine"));
  const enabled = Boolean(inner.enabled ?? outer.enabled ?? false);
  return {
    id,
    name,
    enabled,
    bot_id: typeof inner.bot_id === "string" ? inner.bot_id : undefined,
    instruction: typeof inner.instruction === "string" ? inner.instruction : undefined,
    timezone: typeof inner.timezone === "string" ? inner.timezone : undefined,
    hour: typeof inner.hour === "number" ? inner.hour : undefined,
    minute: typeof inner.minute === "number" ? inner.minute : undefined,
    next_run_utc: typeof outer.next_run_utc === "string" ? outer.next_run_utc : null,
  };
}

export function normalizeActivityPost(raw: unknown): ActivityPost {
  const obj = (raw && typeof raw === "object" ? raw : {}) as Record<string, unknown>;
  const body =
    typeof obj.body === "string"
      ? obj.body
      : typeof obj.content === "string"
        ? obj.content
        : typeof obj.summary === "string"
          ? obj.summary
          : "";
  return {
    id: String(obj.id ?? ""),
    title: String(obj.title ?? "Update"),
    body,
    category: typeof obj.category === "string" ? obj.category : undefined,
    kicker: typeof obj.kicker === "string" ? obj.kicker : undefined,
    created_at: typeof obj.created_at === "string" ? obj.created_at : undefined,
  };
}

export function parseMessageCard(msg: DirectMessage): MessageCard | null {
  if (msg.card && typeof msg.card === "object") {
    return msg.card;
  }
  if (typeof msg.card_json === "string" && msg.card_json.trim()) {
    try {
      const parsed = JSON.parse(msg.card_json);
      if (parsed && typeof parsed === "object") return parsed as MessageCard;
    } catch {
      return null;
    }
  }
  return null;
}

export class DtV1Client {
  private readonly baseUrl: string;
  private workspaceId: string;
  private csrfToken = "";
  private memoryToken = "";
  private readonly activeControllers = new Set<AbortController>();

  constructor(opts: DtV1ClientOptions = {}) {
    this.baseUrl = (opts.baseUrl ?? "").replace(/\/$/, "");
    this.workspaceId = opts.workspaceId ?? "default";
    this.memoryToken = opts.memoryToken ?? "";
  }

  setWorkspaceId(wsId: string) {
    this.workspaceId = wsId;
  }

  getWorkspaceId(): string {
    return this.workspaceId;
  }

  abortAllRequests() {
    for (const controller of this.activeControllers) {
      controller.abort();
    }
    this.activeControllers.clear();
  }

  private async request<T>(path: string, init?: RequestInit, pairingToken?: string): Promise<T> {
    const controller = new AbortController();
    this.activeControllers.add(controller);
    const externalSignal = init?.signal;
    const onExternalAbort = () => controller.abort();
    if (externalSignal) {
      if (externalSignal.aborted) {
        controller.abort();
      } else {
        externalSignal.addEventListener("abort", onExternalAbort, { once: true });
      }
    }
    try {
      const method = (init?.method ?? "GET").toUpperCase();
      const token = pairingToken ?? this.memoryToken;
      const headers: Record<string, string> = {
        "Content-Type": "application/json",
        ...(pairingToken === undefined ? { "X-DT-Workspace-Id": this.workspaceId } : {}),
        ...(init?.headers as Record<string, string> | undefined),
      };
      if (token) {
        headers.Authorization = `Bearer ${token}`;
      }
      if (this.csrfToken && method !== "GET" && method !== "HEAD") {
        headers["X-DT-CSRF-Token"] = this.csrfToken;
      }
      const resp = await fetch(`${this.baseUrl}${buildV1Path(path)}`, {
        ...init,
        signal: controller.signal,
        credentials: token ? "omit" : "include",
        headers,
      });
      if (!resp.ok) {
        const msg = await resp.text();
        throw new Error(`HTTP ${resp.status}: ${msg}`);
      }
      return (await resp.json()) as T;
    } finally {
      this.activeControllers.delete(controller);
      if (externalSignal) {
        externalSignal.removeEventListener("abort", onExternalAbort);
      }
    }
  }

  private isSameOriginLoopback(): boolean {
    const origin = window.location.origin;
    const url = new URL(this.baseUrl || origin, origin);
    return url.origin === origin &&
      (url.hostname === "localhost" || url.hostname === "[::1]" || /^127(?:\.\d+){3}$/.test(url.hostname));
  }

  async connectionMode(signal: AbortSignal): Promise<ConnectionMode> {
    const { hosted_backend } = await this.request<{ hosted_backend: string }>("/health/ready", {
      signal,
      cache: "no-store",
    });
    if (hosted_backend === "sqlite" || hosted_backend === "postgres") return "hosted";
    if (hosted_backend === "local" && this.isSameOriginLoopback()) return "local";
    throw new Error("Open DT on the local loopback address or connect to a hosted DT server.");
  }

  async connectLocal(token: string): Promise<string> {
    if (!this.isSameOriginLoopback() || !token) {
      throw new Error("Local pairing requires a loopback URL and Companion token.");
    }
    const workspace = await this.request<{ active_workspace: string }>("/workspaces", undefined, token);
    this.memoryToken = token;
    this.workspaceId = workspace.active_workspace;
    return workspace.active_workspace;
  }

  async restoreSession(signal?: AbortSignal): Promise<HostedSessionInfo> {
    const session = await this.request<HostedSessionInfo>("/auth/session", { signal });
    this.csrfToken = session.csrf_token;
    this.workspaceId = session.ws_id;
    return session;
  }

  listDirectMessages(botId: string, signal?: AbortSignal) {
    return this.request<DirectMessage[]>(`/dm/${encodeURIComponent(botId)}/messages`, { signal });
  }

  listChannels(signal?: AbortSignal) {
    return this.request<Channel[]>("/channels", { signal });
  }

  createChannel(id: string, topic: string, participants: string[] = []) {
    return this.request<Channel>("/channels", {
      method: "POST",
      body: JSON.stringify({ id, topic, participants }),
    });
  }

  listChannelMessages(channelId: string, signal?: AbortSignal) {
    return this.request<DirectMessage[]>(`/channels/${encodeURIComponent(channelId)}/messages`, {
      signal,
    });
  }

  async listRoutines(signal?: AbortSignal): Promise<Routine[]> {
    const raw = await this.request<unknown[]>("/routines", { signal });
    return Array.isArray(raw) ? raw.map(normalizeRoutine).filter((r) => Boolean(r.id)) : [];
  }

  saveRoutine(routine: Record<string, unknown>) {
    return this.request<Routine>("/routines", {
      method: "POST",
      body: JSON.stringify(routine),
    });
  }

  testRoutine(routineId: string) {
    return this.request<RoutineRun>(`/routines/${encodeURIComponent(routineId)}/test`, {
      method: "POST",
      body: "{}",
    });
  }

  listRoutineRuns(routineId: string, signal?: AbortSignal) {
    return this.request<RoutineRun[]>(`/routines/${encodeURIComponent(routineId)}/runs`, {
      signal,
    });
  }

  async listActivity(signal?: AbortSignal): Promise<{ posts: ActivityPost[]; brief_prompt?: string | null }> {
    const res = await this.request<{ posts?: unknown[]; brief_prompt?: string | null }>("/activity", {
      signal,
    });
    return {
      posts: Array.isArray(res?.posts) ? res.posts.map(normalizeActivityPost) : [],
      brief_prompt: res?.brief_prompt ?? null,
    };
  }

  createActivityPost(title: string, body: string) {
    return this.request<ActivityPost>("/activity", {
      method: "POST",
      body: JSON.stringify({ title, body }),
    });
  }

  listTasks(signal?: AbortSignal) {
    return this.request<Task[]>("/queue", { signal });
  }

  controlTask(taskId: string, action: "pause" | "resume" | "cancel" | "wheel-resume") {
    return this.request<{ ok: boolean }>(
      `/tasks/${encodeURIComponent(taskId)}/${action}`,
      { method: "POST", body: "{}" },
    );
  }

  listGoals(signal?: AbortSignal) {
    return this.request<GoalItem[]>("/goals", { signal });
  }

  createGoal(title: string, description: string, category?: string) {
    return this.request<GoalItem>("/goals", {
      method: "POST",
      body: JSON.stringify({ title, description, category }),
    });
  }

  completeGoal(slug: string) {
    return this.request<Record<string, unknown>>("/goals", {
      method: "PATCH",
      body: JSON.stringify({ slug, action: "complete", verified: true }),
    });
  }

  listFiles(botId: string, signal?: AbortSignal) {
    return this.request<{ files: Array<{ path: string; size: number }> }>(
      `/bots/${encodeURIComponent(botId)}/files`,
      { signal },
    );
  }

  previewFile(botId: string, path: string, signal?: AbortSignal) {
    return this.request<{
      data_url?: string;
      text?: string;
      preview: { kind: string; text_snippet?: string; pdf_pages_estimate?: number };
    }>(`/bots/${encodeURIComponent(botId)}/files/preview?path=${encodeURIComponent(path)}`, {
      signal,
    });
  }

  async redeemEnrollment(enrollmentToken: string): Promise<HostedSessionInfo> {
    const session = await this.request<HostedSessionInfo>("/auth/session", {
      method: "POST",
      body: JSON.stringify({ enrollment_token: enrollmentToken }),
    });
    this.csrfToken = session.csrf_token;
    this.workspaceId = session.ws_id;
    return session;
  }

  async signOut() {
    this.abortAllRequests();
    if (this.memoryToken) {
      this.memoryToken = "";
      return;
    }
    try {
      const res = await this.request<{ signed_out: boolean }>("/auth/logout", {
        method: "POST",
        body: JSON.stringify({}),
      });
      return res;
    } finally {
      this.csrfToken = "";
    }
  }

  listWorkspaces(signal?: AbortSignal) {
    return this.request<Record<string, unknown>>("/workspaces", { signal });
  }

  getDoctorStatus(signal?: AbortSignal) {
    return this.request<Record<string, unknown>>("/doctor", { signal });
  }

  bootstrap(signal?: AbortSignal) {
    return this.request<Record<string, unknown>>("/bootstrap", { signal });
  }

  postDirectMessage(botId: string, content: string, clientMsgId?: string) {
    return this.request<DirectPostOutcome>(`/dm/${encodeURIComponent(botId)}/messages`, {
      method: "POST",
      body: JSON.stringify({
        content,
        ...(clientMsgId ? { client_msg_id: clientMsgId } : {}),
      }),
    });
  }

  getDirectMessages(botId: string, signal?: AbortSignal) {
    return this.listDirectMessages(botId, signal);
  }

  getChannelMessages(channelId: string, signal?: AbortSignal) {
    return this.listChannelMessages(channelId, signal);
  }

  postChannelMessage(channelId: string, content: string) {
    return this.request<Record<string, unknown>>(
      `/channels/${encodeURIComponent(channelId)}/messages`,
      { method: "POST", body: JSON.stringify({ content }) },
    );
  }

  respondApproval(action: ApprovalAction) {
    return this.request<{ request_id: string; approved: boolean; resolved: boolean; status?: string }>(
      `/approvals/${encodeURIComponent(action.request_id)}`,
      {
        method: "POST",
        body: JSON.stringify({
          approved: action.approved,
          ...(action.remember_rule ? { remember_rule: action.remember_rule } : {}),
        }),
      },
    );
  }

  triggerRoutine(routineId: string, deliveryId: string) {
    return this.request<Record<string, unknown>>(
      `/routines/${encodeURIComponent(routineId)}/trigger`,
      { method: "POST", body: JSON.stringify({ delivery_id: deliveryId }) },
    );
  }

  getProviders(signal?: AbortSignal) {
    return this.request<{ profiles?: ProviderProfile[]; workspace_default_profile_id?: string }>(
      "/system/providers",
      { signal },
    );
  }

  providerAction(profileId: string, op: string, payload: Record<string, unknown> = {}) {
    return this.request<Record<string, unknown>>(
      `/system/providers/${encodeURIComponent(profileId)}/oauth/${encodeURIComponent(op)}`,
      { method: "POST", body: JSON.stringify(payload) },
    );
  }

  listSpaces(signal?: AbortSignal) {
    return this.request<Array<{ slug: string; title?: string; published?: boolean }>>("/spaces", {
      signal,
    });
  }

  createSpace(slug: string, title: string, html?: string) {
    return this.request<Record<string, unknown>>("/spaces", {
      method: "POST",
      body: JSON.stringify({ slug, title, kind: "static", ...(html ? { html } : {}) }),
    });
  }

  getSpaceUrl(slug: string, signal?: AbortSignal) {
    return this.request<{ url: string; preview_parent_origin?: string | null }>(
      `/spaces/${encodeURIComponent(slug)}/url`,
      { signal },
    );
  }

  listConnectors(signal?: AbortSignal) {
    return this.request<{ integrations?: ConnectorSummary[]; custom_connectors?: unknown[] }>(
      "/connectors",
      { signal },
    );
  }

  probeIntegration(id: string) {
    return this.request<Record<string, unknown>>(
      `/connectors/probe/${encodeURIComponent(id)}`,
    );
  }

  listMarketplace(signal?: AbortSignal) {
    return this.request<{ catalog?: MarketplaceListing[]; installed?: MarketplaceListing[] }>(
      "/marketplace",
      { signal },
    );
  }

  installMarketplacePlugin(id: string, confirmedScopes: string[]) {
    return this.request<Record<string, unknown>>("/marketplace/install", {
      method: "POST",
      body: JSON.stringify({ id, confirmed_scopes: confirmedScopes }),
    });
  }

  removeMarketplacePlugin(id: string) {
    return this.request<Record<string, unknown>>(`/marketplace/${encodeURIComponent(id)}`, {
      method: "DELETE",
    });
  }

  getBillingStatus(signal?: AbortSignal) {
    return this.request<Record<string, unknown>>("/billing/status", { signal });
  }

  createBillingCheckout(priceId = "price_default") {
    return this.request<Record<string, unknown>>("/billing/checkout", {
      method: "POST",
      body: JSON.stringify({ price_id: priceId }),
    });
  }

  eventsUrl(afterSeq?: number): string {
    return `${this.baseUrl}${workspaceEventsPath(this.workspaceId, afterSeq)}`;
  }

  async subscribeLocalEvents(
    signal: AbortSignal,
    onEvent: (event: string) => void,
    onUnauthorized: () => void,
  ): Promise<void> {
    const url = this.eventsUrl();
    const token = this.memoryToken;
    let lastEventId = "";
    while (!signal.aborted) {
      try {
        const response = await fetch(url, {
          headers: {
            Accept: "text/event-stream",
            Authorization: `Bearer ${token}`,
            ...(lastEventId ? { "Last-Event-ID": lastEventId } : {}),
          },
          signal,
          cache: "no-store",
          credentials: "omit",
          redirect: "error",
        });
        if (response.status === 401 || response.status === 403) {
          onUnauthorized();
          return;
        }
        if (!response.ok || !response.body) throw new Error("DT event stream unavailable");
        onEvent("open");
        const reader = response.body.pipeThrough(new TextDecoderStream()).getReader();
        try {
          let buffer = "";
          while (!signal.aborted) {
            const { done, value } = await reader.read();
            if (done) break;
            buffer += value;
            let boundary: RegExpExecArray | null;
            while ((boundary = /\r?\n\r?\n/.exec(buffer))) {
              const frame = buffer.slice(0, boundary.index);
              buffer = buffer.slice(boundary.index + boundary[0].length);
              const id = /^id: *([1-9]\d*)\r?$/m.exec(frame)?.[1];
              if (id) lastEventId = id;
              const type = /^event: *([^\r\n]+)\r?$/m.exec(frame)?.[1];
              if (type) onEvent(type);
            }
          }
        } finally {
          await reader.cancel().catch(() => {});
          reader.releaseLock();
        }
      } catch {
        // Retry transient disconnects with the last complete event ID.
      }
      if (!signal.aborted) {
        await new Promise<void>((resolve) => {
          const done = () => {
            clearTimeout(timer);
            signal.removeEventListener("abort", done);
            resolve();
          };
          const timer = setTimeout(done, 1000);
          signal.addEventListener("abort", done, { once: true });
        });
      }
    }
  }
}

/**
 * Smart composer transform: converts `->` outside backtick code spans to `→`,
 * and restores `->` when a single Backspace removes a recién-converted `→`.
 */
export function applySmartArrowEdit(previous: string, current: string): string {
  if (
    previous.endsWith("→") &&
    current.length === previous.length - 1 &&
    previous.slice(0, -1) === current
  ) {
    return `${current}->`;
  }
  const parts = current.split(/(`+[^`]*`+)/g);
  return parts
    .map((part) => (part.startsWith("`") ? part : part.replace(/->/g, "→")))
    .join("");
}

export { DtV1Client as MuseV1Client };
