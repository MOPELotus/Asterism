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
