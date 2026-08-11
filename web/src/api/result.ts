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
      error.message,
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
