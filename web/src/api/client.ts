import { client } from "./generated/client.gen.ts";

const configuredBaseUrl = import.meta.env.VITE_ASTERISM_API_BASE_URL?.trim();

client.setConfig({
  baseUrl: configuredBaseUrl || "/",
  credentials: "include",
});

export { client as apiClient };
