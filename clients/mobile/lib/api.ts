import * as SecureStore from "expo-secure-store";

const CREDENTIAL_KEY = "dt_hosted_connection_v1";

export type Connection = { baseUrl: string; token: string; workspaceId: string };
export type Bot = { id: string; display_name?: string; role_title?: string };
export type Channel = { id: string; name?: string; topic?: string };
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
export type Message = {
  id: string;
  author: string;
  role?: string;
  content: string;
  created_at?: string;
  request_id?: string | null;
  card?: MessageCard | null;
  card_json?: string | null;
};
export type DirectPostOutcome = {
  user_message?: Message;
  assistant_message?: Message | null;
  reply_error?: string | null;
  provider_setup_required?: boolean;
};
export type RawRoutineEntry =
  | {
      routine?: {
        id?: string;
        name?: string;
        enabled?: boolean;
        bot_id?: string;
        instruction?: string;
      };
      next_run_utc?: string | null;
      last_run_at?: string | null;
      id?: string;
      name?: string;
      enabled?: boolean;
      bot_id?: string;
      instruction?: string;
    }
  | Record<string, unknown>;
export type Routine = {
  id: string;
  name: string;
  enabled: boolean;
  bot_id?: string;
  instruction?: string;
  next_run_utc?: string | null;
};
export type RoutineRun = { id: string; fired_at: string; status: string; task_id?: string | null };
export type ActivityPost = {
  id: string;
  title: string;
  body: string;
  category?: string;
  kicker?: string;
  created_at?: string;
};
export type ProviderProfileSummary = {
  id: string;
  display_name: string;
  connection_state: string;
  selected_model?: string;
  last_error?: string | null;
};

const activeControllers = new Set<AbortController>();

export function abortAllMobileRequests(): void {
  for (const controller of activeControllers) {
    controller.abort();
  }
  activeControllers.clear();
}

export function createClientMessageId(): string {
  const random =
    typeof crypto !== "undefined" && typeof crypto.randomUUID === "function"
      ? crypto.randomUUID()
      : `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
  return `dm-${random}`;
}

export function normalizeRoutine(raw: RawRoutineEntry): Routine {
  const outer = (raw ?? {}) as Record<string, unknown>;
  const inner =
    outer.routine && typeof outer.routine === "object"
      ? (outer.routine as Record<string, unknown>)
      : outer;
  const id = String(inner.id ?? outer.id ?? "");
  const name = String(inner.name ?? outer.name ?? (id || "Unnamed routine"));
  const enabled = Boolean(inner.enabled ?? outer.enabled ?? false);
  const bot_id =
    typeof inner.bot_id === "string"
      ? inner.bot_id
      : typeof outer.bot_id === "string"
        ? outer.bot_id
        : undefined;
  const instruction =
    typeof inner.instruction === "string"
      ? inner.instruction
      : typeof outer.instruction === "string"
        ? outer.instruction
        : undefined;
  const next_run_utc =
    typeof outer.next_run_utc === "string"
      ? outer.next_run_utc
      : typeof inner.next_run_utc === "string"
        ? inner.next_run_utc
        : null;
  return { id, name, enabled, bot_id, instruction, next_run_utc };
}

export function normalizeActivityPost(raw: Record<string, unknown>): ActivityPost {
  const body =
    typeof raw.body === "string"
      ? raw.body
      : typeof raw.content === "string"
        ? raw.content
        : typeof raw.summary === "string"
          ? raw.summary
          : "";
  return {
    id: String(raw.id ?? ""),
    title: String(raw.title ?? "Activity update"),
    body,
    category: typeof raw.category === "string" ? raw.category : undefined,
    kicker: typeof raw.kicker === "string" ? raw.kicker : undefined,
    created_at: typeof raw.created_at === "string" ? raw.created_at : undefined,
  };
}

export function parseMessageCard(msg: Message): MessageCard | null {
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

export function mobileV1Endpoint(baseUrl: string, path: string): string {
  const root = new URL(baseUrl);
  if (
    root.protocol !== "https:" ||
    root.username ||
    root.password ||
    root.search ||
    root.hash ||
    root.pathname !== "/"
  ) {
    throw new Error("Use the HTTPS origin of your DT server.");
  }
  return `${root.origin}/api/v1/${path.replace(/^\//, "")}`;
}

export async function saveConnection(connection: Connection): Promise<void> {
  await SecureStore.setItemAsync(CREDENTIAL_KEY, JSON.stringify(connection), {
    keychainAccessible: SecureStore.WHEN_UNLOCKED_THIS_DEVICE_ONLY,
  });
}

export async function loadConnection(): Promise<Connection | null> {
  const stored = await SecureStore.getItemAsync(CREDENTIAL_KEY);
  return stored ? (JSON.parse(stored) as Connection) : null;
}

export async function clearConnection(): Promise<void> {
  abortAllMobileRequests();
  await SecureStore.deleteItemAsync(CREDENTIAL_KEY);
}

export async function request<T>(
  connection: Connection,
  path: string,
  init?: RequestInit,
): Promise<T> {
  const controller = new AbortController();
  activeControllers.add(controller);
  const externalSignal = init?.signal;
  const onAbort = () => controller.abort();
  if (externalSignal) {
    if (externalSignal.aborted) {
      controller.abort();
    } else {
      externalSignal.addEventListener("abort", onAbort, { once: true });
    }
  }
  try {
    const response = await fetch(mobileV1Endpoint(connection.baseUrl, path), {
      ...init,
      signal: controller.signal,
      redirect: "error",
      headers: {
        Authorization: `Bearer ${connection.token}`,
        "X-DT-Workspace-Id": connection.workspaceId,
        "Content-Type": "application/json",
        ...(init?.headers as Record<string, string> | undefined),
      },
    });
    if (!response.ok) {
      const detail = await response.text().catch(() => "");
      throw new Error(
        detail.trim()
          ? `DT server returned HTTP ${response.status}: ${detail.trim()}`
          : `DT server returned HTTP ${response.status}.`,
      );
    }
    return (await response.json()) as T;
  } finally {
    activeControllers.delete(controller);
    if (externalSignal) {
      externalSignal.removeEventListener("abort", onAbort);
    }
  }
}

export async function connectedRequest<T>(path: string, init?: RequestInit): Promise<T> {
  const connection = await loadConnection();
  if (!connection) throw new Error("Connect to your DT server in Settings.");
  return request<T>(connection, path, init);
}

export async function listRoutines(signal?: AbortSignal): Promise<Routine[]> {
  const raw = await connectedRequest<RawRoutineEntry[]>("/routines", { signal });
  return Array.isArray(raw) ? raw.map(normalizeRoutine).filter((r) => Boolean(r.id)) : [];
}

export async function listRoutineRuns(
  routineId: string,
  signal?: AbortSignal,
): Promise<RoutineRun[]> {
  return connectedRequest<RoutineRun[]>(
    `/routines/${encodeURIComponent(routineId)}/runs`,
    { signal },
  );
}

export async function listActivity(signal?: AbortSignal): Promise<ActivityPost[]> {
  const res = await connectedRequest<{ posts?: Array<Record<string, unknown>> }>("/activity", {
    signal,
  });
  return Array.isArray(res?.posts) ? res.posts.map(normalizeActivityPost) : [];
}

export async function postDirectMessage(
  botId: string,
  content: string,
  clientMsgId: string,
): Promise<DirectPostOutcome> {
  return connectedRequest<DirectPostOutcome>(`/dm/${encodeURIComponent(botId)}/messages`, {
    method: "POST",
    body: JSON.stringify({ content, client_msg_id: clientMsgId }),
  });
}

export async function respondApproval(
  requestId: string,
  approved: boolean,
): Promise<{ request_id: string; approved: boolean; resolved: boolean; status?: string }> {
  return connectedRequest(`/approvals/${encodeURIComponent(requestId)}`, {
    method: "POST",
    body: JSON.stringify({ approved }),
  });
}

export type RoutineTriggerOutcome = {
  routine_id: string;
  delivery_id: string;
  status: string;
  task_id?: string | null;
};

export type DoctorStatus = {
  schema?: number;
  installation_readiness: string;
  sanitized?: boolean;
  note?: string;
  checks?: Array<{ id: string; category?: string; outcome?: string; summary: string }>;
};

export async function triggerRoutine(
  routineId: string,
  deliveryId: string,
  signal?: AbortSignal,
): Promise<RoutineTriggerOutcome> {
  return connectedRequest<RoutineTriggerOutcome>(
    `/routines/${encodeURIComponent(routineId)}/trigger`,
    {
      method: "POST",
      body: JSON.stringify({ delivery_id: deliveryId }),
      signal,
    },
  );
}

export async function fetchSanitizedDoctor(signal?: AbortSignal): Promise<DoctorStatus> {
  return connectedRequest<DoctorStatus>("/doctor", { signal });
}
