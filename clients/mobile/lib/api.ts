import * as SecureStore from "expo-secure-store";

const CREDENTIAL_KEY = "dt_hosted_connection_v1";

export type Connection = { baseUrl: string; token: string; workspaceId: string };
export type Bot = { id: string; display_name: string };
export type Message = { id: string; author: string; content: string };
export type Routine = { id: string; name: string; enabled: boolean };
export type RoutineRun = { id: string; fired_at: string; status: string };

export function mobileV1Endpoint(baseUrl: string, path: string): string {
  const root = new URL(baseUrl);
  if (root.protocol !== "https:" || root.username || root.password || root.search || root.hash || root.pathname !== "/") {
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
  return stored ? JSON.parse(stored) as Connection : null;
}

export async function clearConnection(): Promise<void> {
  await SecureStore.deleteItemAsync(CREDENTIAL_KEY);
}

export async function request<T>(connection: Connection, path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(mobileV1Endpoint(connection.baseUrl, path), {
    ...init,
    redirect: "error",
    headers: {
      Authorization: `Bearer ${connection.token}`,
      "X-DT-Workspace-Id": connection.workspaceId,
      "Content-Type": "application/json",
    },
  });
  if (!response.ok) throw new Error(`DT server returned HTTP ${response.status}.`);
  return response.json() as Promise<T>;
}

export async function connectedRequest<T>(path: string, init?: RequestInit): Promise<T> {
  const connection = await loadConnection();
  if (!connection) throw new Error("Connect to your DT server in Settings.");
  return request<T>(connection, path, init);
}
