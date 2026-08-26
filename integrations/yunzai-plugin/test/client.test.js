import assert from "node:assert/strict"
import test from "node:test"

import { AsterismApiError, AsterismClient } from "../model/client.js"

test("client sends bearer token, filters and idempotency key", async () => {
  const calls = []
  const fetchImpl = async (url, options) => {
    calls.push({ url: String(url), options })
    return new Response(JSON.stringify({ execution: { id: "execution-1" }, created: true }), {
      status: 201,
      headers: { "content-type": "application/json", "x-request-id": "request-1" },
    })
  }
  const client = new AsterismClient({ apiUrl: "http://asterism.test", token: "ast_st_secret", fetchImpl })
  await client.execute("task/unsafe", ["resource_execution"], "idem-1")
  assert.equal(calls[0].url, "http://asterism.test/api/v1/tasks/task%2Funsafe/execute")
  assert.equal(calls[0].options.headers.get("authorization"), "Bearer ast_st_secret")
  assert.equal(calls[0].options.headers.get("idempotency-key"), "idem-1")
  assert.deepEqual(JSON.parse(calls[0].options.body), { requested_capabilities: ["resource_execution"] })
})

test("health never sends the bearer token", async () => {
  let authorization
  const client = new AsterismClient({
    apiUrl: "http://asterism.test",
    token: "ast_st_secret",
    fetchImpl: async (_url, options) => {
      authorization = options.headers.get("authorization")
      return new Response('{"status":"ok"}', { status: 200 })
    },
  })
  await client.health()
  assert.equal(authorization, null)
})

test("QQ assertion uses the gateway token and exact sender identity", async () => {
  let request
  const client = new AsterismClient({
    apiUrl: "http://asterism.test",
    token: "ast_st_gateway",
    fetchImpl: async (url, options) => {
      request = { url: String(url), options }
      return new Response('{"user_id":"u1","qq":"123456","access_token":"ast_ws_user","web_login_path":"/auth/qq/ticket"}', { status: 200 })
    },
  })
  await client.assertQq("123456")
  assert.equal(request.url, "http://asterism.test/api/v1/integrations/qq/identity/assert")
  assert.equal(request.options.headers.get("authorization"), "Bearer ast_st_gateway")
  assert.deepEqual(JSON.parse(request.options.body), {
    qq: "123456",
    create_if_missing: true,
    return_to: "/",
    master_assertion: false,
  })
})

test("QQ assertion carries only the trusted Yunzai master attestation requested by the plugin", async () => {
  let body
  const client = new AsterismClient({
    apiUrl: "http://asterism.test",
    token: "ast_st_gateway",
    fetchImpl: async (_url, options) => {
      body = JSON.parse(options.body)
      return new Response('{"user_id":"u1","qq":"123456","access_token":"ast_ws_user"}', { status: 200 })
    },
  })
  await client.assertQq("123456", true, "/", true)
  assert.equal(body.master_assertion, true)
})

test("QQ assertion preserves the safe confirmation deep-link without exposing the user bearer", async () => {
  let request
  const client = new AsterismClient({
    apiUrl: "http://asterism.test",
    token: "ast_st_gateway",
    fetchImpl: async (url, options) => {
      request = { url: String(url), options }
      return new Response(JSON.stringify({
        user_id: "u1",
        qq: "123456",
        access_token: "ast_ws_user",
        web_login_path: "/api/v1/integrations/qq/web-login/ticket-secret",
      }), { status: 200 })
    },
  })
  const result = await client.assertQq("123456", true, "/tasks/formal-1?confirm=1")
  assert.equal(JSON.parse(request.options.body).return_to, "/tasks/formal-1?confirm=1")
  assert.equal(result.web_login_path.includes(result.access_token), false)
})

test("API errors expose only sanitized code and request id", async () => {
  const client = new AsterismClient({
    apiUrl: "http://asterism.test",
    token: "ast_st_secret",
    fetchImpl: async () => new Response(JSON.stringify({ error: { code: "task_conflict", message: "safe" } }), {
      status: 409,
      headers: { "x-request-id": "request-2" },
    }),
  })
  await assert.rejects(client.task("task-1"), (error) => {
    assert.ok(error instanceof AsterismApiError)
    assert.equal(error.status, 409)
    assert.equal(error.code, "task_conflict")
    assert.equal(error.requestId, "request-2")
    return true
  })
})

test("notification claim/report stay on the gateway token and report only delivery state", async () => {
  const calls = []
  const client = new AsterismClient({
    apiUrl: "http://asterism.test",
    token: "ast_st_gateway",
    fetchImpl: async (url, options) => {
      calls.push({ url: String(url), options })
      return new Response(options.method === "POST" && String(url).endsWith("/claim") ? '{"items":[]}' : "", { status: 200 })
    },
  })
  await client.claimQqNotifications()
  await client.reportQqNotifications([{ id: "n1", delivered: true }])
  assert.equal(calls.length, 2)
  assert.ok(calls[0].url.endsWith("/notifications/claim"))
  assert.ok(calls[1].url.endsWith("/notifications/report"))
  assert.equal(calls[0].options.headers.get("authorization"), "Bearer ast_st_gateway")
  assert.deepEqual(JSON.parse(calls[1].options.body), { items: [{ id: "n1", delivered: true }] })
})

test("delegated gateway clients bind the explicit target owner header", async () => {
  let targetOwner
  const client = new AsterismClient({
    apiUrl: "http://asterism.test",
    token: "ast_st_gateway",
    targetOwnerId: "owner-2",
    fetchImpl: async (_url, options) => {
      targetOwner = options.headers.get("x-asterism-target-owner")
      return new Response('{"total":0,"items":[]}', { status: 200 })
    },
  })
  await client.accounts()
  assert.equal(targetOwner, "owner-2")
})
