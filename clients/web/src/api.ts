export type ApprovalAction = {
  request_id: string;
  approved: boolean;
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

export class DtV1Client {
  private readonly baseUrl: string;
  private workspaceId: string;
  private csrfToken = "";
  private memoryToken = "";

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

  private async request<T>(path: string, init?: RequestInit): Promise<T> {
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
      credentials: "include",
      headers,
    });
    if (!resp.ok) {
      const msg = await resp.text();
      throw new Error(`HTTP ${resp.status}: ${msg}`);
    }
    return (await resp.json()) as T;
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
    const res = await this.request<{ signed_out: boolean }>("/auth/logout", {
      method: "POST",
      body: JSON.stringify({}),
    });
    this.csrfToken = "";
    return res;
  }

  listWorkspaces() {
    return this.request<Record<string, unknown>>("/workspaces");
  }

  getDoctorStatus() {
    return this.request<Record<string, unknown>>("/doctor");
  }

  bootstrap() {
    return this.request<Record<string, unknown>>("/bootstrap");
  }

  getDirectMessages(botId: string) {
    return this.request<Record<string, unknown> | Array<Record<string, unknown>>>(
      `/dm/${encodeURIComponent(botId)}/messages`,
    );
  }

  postDirectMessage(botId: string, content: string) {
    return this.request<Record<string, unknown>>(
      `/dm/${encodeURIComponent(botId)}/messages`,
      { method: "POST", body: JSON.stringify({ content }) },
    );
  }

  getChannelMessages(channelId: string) {
    return this.request<Array<Record<string, unknown>>>(
      `/channels/${encodeURIComponent(channelId)}/messages`,
    );
  }

  postChannelMessage(channelId: string, content: string) {
    return this.request<Record<string, unknown>>(
      `/channels/${encodeURIComponent(channelId)}/messages`,
      { method: "POST", body: JSON.stringify({ content }) },
    );
  }

  respondApproval(action: ApprovalAction) {
    return this.request<Record<string, unknown>>(
      `/approvals/${encodeURIComponent(action.request_id)}`,
      { method: "POST", body: JSON.stringify({ approved: action.approved }) },
    );
  }

  listRoutines() {
    return this.request<Array<Record<string, unknown>>>("/routines");
  }

  triggerRoutine(routineId: string, deliveryId: string) {
    return this.request<Record<string, unknown>>(
      `/routines/${encodeURIComponent(routineId)}/trigger`,
      { method: "POST", body: JSON.stringify({ delivery_id: deliveryId }) },
    );
  }

  getProviders() {
    return this.request<Record<string, unknown>>("/system/providers");
  }

  listSpaces() {
    return this.request<unknown[]>("/spaces");
  }

  createSpace(slug: string, title: string, html?: string) {
    return this.request<Record<string, unknown>>("/spaces", {
      method: "POST",
      body: JSON.stringify({ slug, title, kind: "static", html }),
    });
  }

  getSpaceUrl(slug: string) {
    return this.request<{ url: string }>(`/spaces/${encodeURIComponent(slug)}/url`);
  }

  probeIntegration(id: string) {
    return this.request<Record<string, unknown>>(
      `/connectors/probe/${encodeURIComponent(id)}`,
    );
  }

  listMarketplace() {
    return this.request<Record<string, unknown>>("/marketplace");
  }

  getBillingStatus() {
    return this.request<Record<string, unknown>>("/billing/status");
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
