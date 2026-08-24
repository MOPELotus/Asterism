const DEFAULT_TIMEOUT_MS = 180_000

export function readConfig(env = process.env) {
  const apiUrl = normalizeUrl(env.ASTERISM_URL || "http://127.0.0.1:8068")
  const webUrl = normalizeUrl(env.ASTERISM_WEB_URL || apiUrl)
  const token = String(env.ASTERISM_TOKEN || "").trim()
  const allowedQqs = new Set(
    String(env.ASTERISM_ALLOWED_QQ || "")
      .split(",")
      .map((value) => value.trim())
      .filter((value) => /^\d{5,20}$/.test(value)),
  )
  const requestTimeoutMs = parseTimeout(env.ASTERISM_REQUEST_TIMEOUT_MS)
  return {
    apiUrl,
    webUrl,
    token,
    allowedQqs,
    allowGroups: /^(1|true|yes)$/i.test(String(env.ASTERISM_ALLOW_GROUPS || "")),
    requestTimeoutMs,
  }
}

export function validateConfig(config) {
  if (!config.token.startsWith("ast_st_")) {
    throw new Error("ASTERISM_TOKEN 必须是 owner-bound Service Token")
  }
  if (config.allowedQqs.size === 0) {
    throw new Error("ASTERISM_ALLOWED_QQ 至少需要配置一个 QQ 号")
  }
}

function normalizeUrl(value) {
  const parsed = new URL(String(value).trim())
  if (!/^https?:$/.test(parsed.protocol)) throw new Error("Asterism URL 只支持 HTTP(S)")
  return parsed.toString().replace(/\/$/, "")
}

function parseTimeout(value) {
  if (value == null || String(value).trim() === "") return DEFAULT_TIMEOUT_MS
  const parsed = Number(value)
  if (!Number.isSafeInteger(parsed) || parsed < 1_000 || parsed > 600_000) {
    throw new Error("ASTERISM_REQUEST_TIMEOUT_MS 必须在 1000-600000 之间")
  }
  return parsed
}
