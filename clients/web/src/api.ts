export type ApprovalAction = {
  request_id: string;
  approved: boolean;
};

export type DirectMessage = { id: string; author: string; content: string };
export type Routine = { id: string; name: string; enabled: boolean };
export type Task = { id: string; instruction: string; status: string };

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

  async restoreSession(): Promise<HostedSessionInfo> {
    const session = await this.request<HostedSessionInfo>("/auth/session");
    this.csrfToken = session.csrf_token;
    this.workspaceId = session.ws_id;
    return session;
  }

  listDirectMessages(botId: string, signal?: AbortSignal) {
    return this.request<DirectMessage[]>(`/dm/${encodeURIComponent(botId)}/messages`, { signal });
  }

  listRoutines() {
    return this.request<Routine[]>("/routines");
  }

  listActivity() {
    return this.request<{ posts: Array<{ id: string; title?: string; content?: string }> }>("/activity");
  }

  listTasks() {
    return this.request<Task[]>("/queue");
  }

  controlTask(taskId: string, action: "pause" | "resume" | "cancel" | "wheel-resume") {
    return this.request<{ ok: boolean }>(
      `/tasks/${encodeURIComponent(taskId)}/${action}`,
      { method: "POST", body: "{}" },
    );
  }

  listFiles(botId: string) {
    return this.request<{ files: Array<{ path: string; size: number }> }>(
      `/bots/${encodeURIComponent(botId)}/files`,
    );
  }

  previewFile(botId: string, path: string) {
    return this.request<{ data_url?: string; preview: { kind: string; text_snippet?: string; pdf_pages_estimate?: number } }>(
      `/bots/${encodeURIComponent(botId)}/files/preview?path=${encodeURIComponent(path)}`,
    );
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

  postDirectMessage(botId: string, content: string) {
    return this.request<Record<string, unknown>>(
      `/dm/${encodeURIComponent(botId)}/messages`,
      { method: "POST", body: JSON.stringify({ content }) },
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

  getProviders() {
    return this.request<Record<string, unknown>>("/system/providers");
  }

  listSpaces() {
    return this.request<unknown[]>("/spaces");
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

export { DtV1Client as MuseV1Client };
