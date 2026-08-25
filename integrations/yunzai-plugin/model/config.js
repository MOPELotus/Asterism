import fs from "node:fs"
import path from "node:path"
import { fileURLToPath } from "node:url"

const DEFAULT_TIMEOUT_MS = 180_000
const PLUGIN_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..")
const CONFIG_FILE = path.join(PLUGIN_ROOT, "config", "asterism.json")

export function readConfig(env = process.env) {
  const stored = readStoredConfig()
  const apiUrl = normalizeUrl(env.ASTERISM_URL || stored.apiUrl || "http://127.0.0.1:8068")
  const webUrl = normalizeUrl(env.ASTERISM_WEB_URL || stored.webUrl || apiUrl)
  const token = String(env.ASTERISM_TOKEN || stored.token || "").trim()
  return {
    apiUrl,
    webUrl,
    token,
    allowedGroups: parseIds(env.ASTERISM_ALLOWED_GROUPS ?? stored.allowedGroups),
    notificationGroups: parseIds(env.ASTERISM_NOTIFICATION_GROUPS ?? stored.notificationGroups),
    notificationIntervalMs: parseNotificationInterval(env.ASTERISM_NOTIFICATION_INTERVAL_MS ?? stored.notificationIntervalMs),
    adminContact: String(env.ASTERISM_ADMIN_CONTACT || stored.adminContact || "").trim(),
    requestTimeoutMs: parseTimeout(env.ASTERISM_REQUEST_TIMEOUT_MS ?? stored.requestTimeoutMs),
  }
}

export function writeConfig(value) {
  const config = {
    apiUrl: normalizeUrl(value.apiUrl || "http://127.0.0.1:8068"),
    webUrl: normalizeUrl(value.webUrl || value.apiUrl || "http://127.0.0.1:5173"),
    token: String(value.token || "").trim(),
    allowedGroups: [...parseIds(value.allowedGroups)],
    notificationGroups: [...parseIds(value.notificationGroups)],
    notificationIntervalMs: parseNotificationInterval(value.notificationIntervalMs),
    adminContact: String(value.adminContact || "").trim(),
    requestTimeoutMs: parseTimeout(value.requestTimeoutMs),
  }
  fs.mkdirSync(path.dirname(CONFIG_FILE), { recursive: true })
  const temporary = `${CONFIG_FILE}.${process.pid}.tmp`
  fs.writeFileSync(temporary, `${JSON.stringify(config, null, 2)}\n`, { encoding: "utf8", mode: 0o600 })
  fs.renameSync(temporary, CONFIG_FILE)
  return config
}

export function validateConfig(config) {
  if (!config.token.startsWith("ast_st_")) {
    throw new Error("Asterism 服务令牌未配置；令牌必须包含 qq_identity_assert 权限")
  }
}

function readStoredConfig() {
  try {
    return JSON.parse(fs.readFileSync(CONFIG_FILE, "utf8"))
  } catch (error) {
    if (error?.code === "ENOENT") return {}
    throw new Error(`无法读取 Asterism 插件配置：${error.message}`)
  }
}

function normalizeUrl(value) {
  const parsed = new URL(String(value).trim())
  if (!/^https?:$/.test(parsed.protocol)) throw new Error("Asterism URL 只支持 HTTP(S)")
  return parsed.toString().replace(/\/$/, "")
}

function parseIds(value) {
  const values = Array.isArray(value) || value instanceof Set ? [...value] : String(value || "").split(/[\s,]+/)
  return new Set(values.map((item) => String(item).trim()).filter((item) => /^\d{5,20}$/.test(item)))
}

function parseTimeout(value) {
  if (value == null || String(value).trim() === "") return DEFAULT_TIMEOUT_MS
  const parsed = Number(value)
  if (!Number.isSafeInteger(parsed) || parsed < 1_000 || parsed > 600_000) {
    throw new Error("requestTimeoutMs 必须在 1000-600000 之间")
  }
  return parsed
}

function parseNotificationInterval(value) {
  if (value == null || String(value).trim() === "") return 30_000
  const parsed = Number(value)
  if (!Number.isSafeInteger(parsed) || parsed < 5_000 || parsed > 3_600_000) {
    throw new Error("notificationIntervalMs 必须在 5000-3600000 之间")
  }
  return parsed
}
