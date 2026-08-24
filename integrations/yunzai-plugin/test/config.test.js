import assert from "node:assert/strict"
import test from "node:test"

import { readConfig, validateConfig } from "../model/config.js"

test("configuration is fail-closed and normalizes URLs", () => {
  const config = readConfig({
    ASTERISM_URL: "http://127.0.0.1:8068/",
    ASTERISM_TOKEN: "ast_st_example",
    ASTERISM_ALLOWED_QQ: "123456, 789012,invalid",
  })
  validateConfig(config)
  assert.equal(config.apiUrl, "http://127.0.0.1:8068")
  assert.deepEqual([...config.allowedQqs], ["123456", "789012"])
  assert.equal(config.allowGroups, false)
})

test("configuration rejects missing access boundaries", () => {
  assert.throws(() => validateConfig(readConfig({ ASTERISM_ALLOWED_QQ: "123456" })), /ASTERISM_TOKEN/)
  assert.throws(() => validateConfig(readConfig({ ASTERISM_TOKEN: "ast_st_example" })), /ASTERISM_ALLOWED_QQ/)
})
