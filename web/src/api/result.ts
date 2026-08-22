import type { ErrorResponse } from "./generated/types.gen.ts";

type ApiResult<T> = {
  data?: T;
  error?: unknown;
  response?: Response;
};

export class AsterismApiError extends Error {
  readonly code: string;
  readonly statusCode: number;

  constructor(statusCode: number, code: string, message: string) {
    super(message);
    this.name = "AsterismApiError";
    this.statusCode = statusCode;
    this.code = code;
  }
}

export function requireData<T>(result: ApiResult<T>): T {
  ensureSuccess(result);
  if (result.data === undefined) {
    throw new AsterismApiError(
      result.response?.status ?? 0,
      "missing_response_data",
      "Asterism API 返回了缺少数据的成功响应",
    );
  }
  return result.data;
}

export function ensureSuccess(result: ApiResult<unknown>): void {
  if (result.error) {
    const error = isErrorResponse(result.error)
      ? result.error.error
      : { code: "api_request_failed", message: "Asterism API 请求失败" };
    throw new AsterismApiError(
      result.response?.status ?? 0,
      error.code,
      localizedApiMessage(error.code, error.message),
    );
  }
  if (result.response && !result.response.ok) {
    throw new AsterismApiError(
      result.response.status,
      "unexpected_http_status",
      `Asterism API 返回 HTTP ${result.response.status}`,
    );
  }
}

function localizedApiMessage(code: string, fallback: string): string {
  const messages: Record<string, string> = {
    provider_authentication_invalid: "平台登录响应发生变化，当前版本暂时无法识别。请稍后重试；若仍失败，无需反复输入凭据。",
    provider_inventory_invalid: "平台返回的课程或任务结构发生变化，当前巡查无法安全继续。账号凭据仍然有效。",
    provider_credential_rejected: "平台拒绝了当前账号或密码，请核对后重试。",
    provider_action_required: "平台要求完成验证码、扫码或其他人工操作，请按照认证区域的提示继续。",
    provider_unavailable: "平台服务当前不可用，请稍后重试。",
  };
  return messages[code] ?? fallback;
}

function isErrorResponse(value: unknown): value is ErrorResponse {
  if (!value || typeof value !== "object" || !("error" in value)) return false;
  const body = value.error;
  return Boolean(
    body &&
      typeof body === "object" &&
      "code" in body &&
      typeof body.code === "string" &&
      "message" in body &&
      typeof body.message === "string",
  );
}
