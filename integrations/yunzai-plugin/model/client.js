export class AsterismApiError extends Error {
  constructor(status, code, message, requestId) {
    super(message)
    this.name = "AsterismApiError"
    this.status = status
    this.code = code
    this.requestId = requestId
  }
}

export class AsterismClient {
  constructor({ apiUrl, token, requestTimeoutMs = 180_000, fetchImpl = globalThis.fetch }) {
    this.apiUrl = apiUrl.replace(/\/$/, "")
    this.token = token
    this.requestTimeoutMs = requestTimeoutMs
    this.fetchImpl = fetchImpl
  }

  health() {
    return this.request("/api/v1/system/health", { authenticated: false })
  }

  identity() {
    return this.request("/api/v1/auth/session")
  }

  assertQq(qq, createIfMissing = true, returnTo = "/") {
    return this.request("/api/v1/integrations/qq/identity/assert", {
      method: "POST",
      body: { qq: String(qq), create_if_missing: createIfMissing, return_to: returnTo },
    })
  }

  claimQqNotifications() {
    return this.request("/api/v1/integrations/qq/notifications/claim", { method: "POST" })
  }

  reportQqNotifications(items) {
    return this.request("/api/v1/integrations/qq/notifications/report", {
      method: "POST",
      body: { items },
    })
  }

  accounts() {
    return this.request("/api/v1/provider-accounts")
  }

  courses(accountId) {
    return this.request("/api/v1/courses", { query: { provider_account_id: accountId, limit: 200 } })
  }

  tasks({ accountId, courseId, limit = 200, offset = 0 } = {}) {
    return this.request("/api/v1/tasks", {
      query: { provider_account_id: accountId, course_id: courseId, limit, offset },
    })
  }

  task(taskId) {
    return this.request(`/api/v1/tasks/${encodeURIComponent(taskId)}`)
  }

  scan(accountId) {
    return this.request(`/api/v1/provider-accounts/${encodeURIComponent(accountId)}/scan`, {
      method: "POST",
    })
  }

  execute(taskId, requestedCapabilities, idempotencyKey) {
    return this.request(`/api/v1/tasks/${encodeURIComponent(taskId)}/execute`, {
      method: "POST",
      headers: { "Idempotency-Key": idempotencyKey },
      body: { requested_capabilities: requestedCapabilities },
    })
  }

  execution(executionId) {
    return this.request(`/api/v1/executions/${encodeURIComponent(executionId)}`)
  }

  async request(path, options = {}) {
    const controller = new AbortController()
    const timeout = setTimeout(() => controller.abort(), this.requestTimeoutMs)
    const url = new URL(`${this.apiUrl}${path}`)
    for (const [key, value] of Object.entries(options.query || {})) {
      if (value != null && value !== "") url.searchParams.set(key, String(value))
    }
    const headers = new Headers(options.headers)
    headers.set("Accept", "application/json")
    if (options.authenticated !== false) headers.set("Authorization", `Bearer ${this.token}`)
    if (options.body !== undefined) headers.set("Content-Type", "application/json")
    try {
      const response = await this.fetchImpl(url, {
        method: options.method || "GET",
        headers,
        body: options.body === undefined ? undefined : JSON.stringify(options.body),
        signal: controller.signal,
      })
      const requestId = response.headers.get("x-request-id") || undefined
      const payload = response.status === 204 ? undefined : await readPayload(response)
      if (!response.ok) {
        const code = payload?.error?.code || `http_${response.status}`
        const message = payload?.error?.message || `Asterism 请求失败 (${response.status})`
        throw new AsterismApiError(response.status, code, message, requestId)
      }
      return payload
    } catch (error) {
      if (error?.name === "AbortError") {
        throw new AsterismApiError(0, "request_timeout", "Asterism 请求超时")
      }
      throw error
    } finally {
      clearTimeout(timeout)
    }
  }
}

async function readPayload(response) {
  const text = await response.text()
  if (!text) return undefined
  try {
    return JSON.parse(text)
  } catch {
    throw new AsterismApiError(response.status, "invalid_response", "Asterism 返回了非 JSON 响应")
  }
}
