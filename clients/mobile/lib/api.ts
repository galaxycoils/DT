import * as SecureStore from "expo-secure-store";

const CREDENTIAL_KEY = "dt_hosted_token_v1";

export function mobileV1Endpoint(baseUrl: string, path: string): string {
  const root = baseUrl.replace(/\/$/, "");
  const clean = path.startsWith("/") ? path : `/${path}`;
  return `${root}/api/v1${clean}`;
}

export async function saveCredentialSecurely(token: string): Promise<void> {
  await SecureStore.setItemAsync(CREDENTIAL_KEY, token, {
    keychainAccessible: SecureStore.WHEN_UNLOCKED_THIS_DEVICE_ONLY,
  });
}

export async function loadCredentialSecurely(): Promise<string | null> {
  return SecureStore.getItemAsync(CREDENTIAL_KEY);
}

export async function clearCredentialSecurely(): Promise<void> {
  await SecureStore.deleteItemAsync(CREDENTIAL_KEY);
}

export async function fetchBootstrap(baseUrl: string, token: string, workspaceId = "default") {
  const resp = await fetch(mobileV1Endpoint(baseUrl, "/bootstrap"), {
    headers: {
      Authorization: `Bearer ${token}`,
      "X-DT-Workspace-Id": workspaceId,
    },
  });
  if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
  return resp.json();
}

export async function postDirectMessage(
  baseUrl: string,
  token: string,
  botId: string,
  content: string,
  workspaceId = "default",
) {
  const resp = await fetch(
    mobileV1Endpoint(baseUrl, `/dm/${encodeURIComponent(botId)}/messages`),
    {
      method: "POST",
      headers: {
        Authorization: `Bearer ${token}`,
        "Content-Type": "application/json",
        "X-DT-Workspace-Id": workspaceId,
      },
      body: JSON.stringify({ content }),
    },
  );
  if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
  return resp.json();
}

export async function respondApproval(
  baseUrl: string,
  token: string,
  requestId: string,
  approved: boolean,
  workspaceId = "default",
) {
  const resp = await fetch(
    mobileV1Endpoint(baseUrl, `/approvals/${encodeURIComponent(requestId)}`),
    {
      method: "POST",
      headers: {
        Authorization: `Bearer ${token}`,
        "Content-Type": "application/json",
        "X-DT-Workspace-Id": workspaceId,
      },
      body: JSON.stringify({ approved }),
    },
  );
  if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
  return resp.json();
}

export async function triggerRoutine(
  baseUrl: string,
  token: string,
  routineId: string,
  deliveryId: string,
  workspaceId = "default",
) {
  const resp = await fetch(
    mobileV1Endpoint(baseUrl, `/routines/${encodeURIComponent(routineId)}/trigger`),
    {
      method: "POST",
      headers: {
        Authorization: `Bearer ${token}`,
        "Content-Type": "application/json",
        "X-DT-Workspace-Id": workspaceId,
      },
      body: JSON.stringify({ delivery_id: deliveryId }),
    },
  );
  if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
  return resp.json();
}

export async function fetchSanitizedDoctor(baseUrl: string, token: string) {
  const resp = await fetch(mobileV1Endpoint(baseUrl, "/doctor"), {
    headers: { Authorization: `Bearer ${token}` },
  });
  if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
  return resp.json();
}
