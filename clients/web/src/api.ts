export type ApprovalAction = {
  request_id: string;
  approved: boolean;
};

export interface MuseV1ClientOptions {
  baseUrl?: string;
  token: string;
  workspaceId?: string;
}

export function buildV1Path(suffix: string): string {
  const clean = suffix.startsWith("/") ? suffix : `/${suffix}`;
  return `/api/v1${clean}`;
}

export function workspaceEventsPath(workspaceId: string): string {
  return buildV1Path(`/workspaces/${encodeURIComponent(workspaceId)}/events`);
}

export class MuseV1Client {
  private readonly baseUrl: string;
  private readonly token: string;
  private readonly workspaceId: string;

  constructor(opts: MuseV1ClientOptions) {
    this.baseUrl = (opts.baseUrl ?? "").replace(/\/$/, "");
    this.token = opts.token;
    this.workspaceId = opts.workspaceId ?? "default";
  }

  private async request<T>(path: string, init?: RequestInit): Promise<T> {
    const resp = await fetch(`${this.baseUrl}${buildV1Path(path)}`, {
      ...init,
      headers: {
        Authorization: `Bearer ${this.token}`,
        "Content-Type": "application/json",
        ...(init?.headers ?? {}),
      },
    });
    if (!resp.ok) {
      const msg = await resp.text();
      throw new Error(`HTTP ${resp.status}: ${msg}`);
    }
    return (await resp.json()) as T;
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

  /** Inline approvals never block sending messages on the same thread. */
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

  eventsUrl(): string {
    return `${this.baseUrl}${workspaceEventsPath(this.workspaceId)}`;
  }
}
