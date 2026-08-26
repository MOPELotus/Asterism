import { client } from "./generated/client.gen.ts";

const configuredBaseUrl = import.meta.env.VITE_ASTERISM_API_BASE_URL?.trim();

client.setConfig({
  baseUrl: configuredBaseUrl || "/",
  credentials: "include",
});

const TARGET_OWNER_STORAGE_KEY = "asterism.target-owner-user-id";

export function setTargetOwnerUserId(userId: string | null) {
  const normalized = userId?.trim() || null;
  if (normalized) localStorage.setItem(TARGET_OWNER_STORAGE_KEY, normalized);
  else localStorage.removeItem(TARGET_OWNER_STORAGE_KEY);
  client.setConfig({ headers: normalized ? { "X-Asterism-Target-Owner": normalized } : {} });
}

export function getTargetOwnerUserId(): string | null {
  if (typeof localStorage === "undefined") return null;
  return localStorage.getItem(TARGET_OWNER_STORAGE_KEY);
}

if (typeof localStorage !== "undefined") setTargetOwnerUserId(getTargetOwnerUserId());

export { client as apiClient };
