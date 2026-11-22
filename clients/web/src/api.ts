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

  private async request<T>(path: string, init?: RequestInit): Promise<T> {
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
      const headers: Record<string, string> = {
        "Content-Type": "application/json",
        "X-DT-Workspace-Id": this.workspaceId,
        ...(init?.headers as Record<string, string> | undefined),
      };
      if (this.memoryToken) {
        headers.Authorization = `Bearer ${this.memoryToken}`;
      }
      if (this.csrfToken && method !== "GET" && method !== "HEAD") {
        headers["X-DT-CSRF-Token"] = this.csrfToken;
      }
      const resp = await fetch(`${this.baseUrl}${buildV1Path(path)}`, {
        ...init,
        signal: controller.signal,
        credentials: "include",
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

  getSpaceUrl(slug: string) {
    return this.request<{ url: string }>(`/spaces/${encodeURIComponent(slug)}/url`);
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
