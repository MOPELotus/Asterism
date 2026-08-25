import assert from "node:assert/strict"
import test from "node:test"

import { readConfig, validateConfig } from "../model/config.js"

test("configuration is fail-closed and normalizes URLs", () => {
  const config = readConfig({
    ASTERISM_URL: "http://127.0.0.1:8068/",
    ASTERISM_TOKEN: "ast_st_example",
    ASTERISM_ALLOWED_GROUPS: "123456, 789012,invalid",
  })
  validateConfig(config)
  assert.equal(config.apiUrl, "http://127.0.0.1:8068")
  assert.deepEqual([...config.allowedGroups], ["123456", "789012"])
  assert.deepEqual([...config.notificationGroups], [])
  assert.equal(config.notificationIntervalMs, 30000)
})

test("configuration rejects missing access boundaries", () => {
  assert.throws(() => validateConfig(readConfig({ ASTERISM_ALLOWED_GROUPS: "123456" })), /服务令牌/)
  assert.doesNotThrow(() => validateConfig(readConfig({ ASTERISM_TOKEN: "ast_st_example" })))
})
