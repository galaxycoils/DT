export function mobileV1Endpoint(baseUrl: string, path: string): string {
  const root = baseUrl.replace(/\/$/, "");
  const clean = path.startsWith("/") ? path : `/${path}`;
  return `${root}/api/v1${clean}`;
}

export async function fetchBootstrap(baseUrl: string, token: string) {
  const resp = await fetch(mobileV1Endpoint(baseUrl, "/bootstrap"), {
    headers: { Authorization: `Bearer ${token}` },
  });
  if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
  return resp.json();
}
