import type { AuthProvider } from "@refinedev/core";

import "@/api/client.ts";
import { currentIdentity, login, logout } from "@/api/generated/sdk.gen.ts";
import type { IdentityResponse } from "@/api/generated/types.gen.ts";
import { AsterismApiError, ensureSuccess, requireData } from "@/api/result.ts";

export type WebIdentity = IdentityResponse & {
  id: string;
  name: string;
};

async function readIdentity(): Promise<IdentityResponse> {
  return requireData(await currentIdentity());
}

export const authProvider: AuthProvider = {
  login: async ({ username, password }: { username?: string; password?: string }) => {
    if (!username?.trim() || !password) {
      return {
        success: false,
        error: new Error("请输入用户名和密码"),
      };
    }
    try {
      requireData(
        await login({
          body: { username: username.trim(), password },
        }),
      );
      return { success: true, redirectTo: "/" };
    } catch (error) {
      return { success: false, error: normalizeError(error) };
    }
  },
  logout: async () => {
    try {
      ensureSuccess(await logout());
    } catch (error) {
      if (!(error instanceof AsterismApiError) || error.statusCode !== 401) {
        return { success: false, error: normalizeError(error) };
      }
    }
    return { success: true, redirectTo: "/login" };
  },
  check: async () => {
    try {
      await readIdentity();
      return { authenticated: true };
    } catch (error) {
      return {
        authenticated: false,
        logout: false,
        redirectTo: "/login",
        error: normalizeError(error),
      };
    }
  },
  onError: async (error) => {
    if (error instanceof AsterismApiError && error.statusCode === 401) {
      return { logout: true, redirectTo: "/login", error };
    }
    return { error: normalizeError(error) };
  },
  getPermissions: async () => (await readIdentity()).permissions,
  getIdentity: async (): Promise<WebIdentity> => {
    const identity = await readIdentity();
    const id = identity.user_id ?? identity.service_token_id ?? identity.identity_type;
    return {
      ...identity,
      id,
      name: identity.identity_type === "web_session" ? "Web Session" : "Service Token",
    };
  },
};

function normalizeError(error: unknown): Error {
  return error instanceof Error ? error : new Error("发生未知错误");
}
