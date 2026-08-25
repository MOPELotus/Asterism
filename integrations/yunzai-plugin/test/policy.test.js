import assert from "node:assert/strict"
import test from "node:test"

import { executionBlockReason, recommendedExecutionCapabilities } from "../model/policy.js"

test("plain upstream execution chooses the same capability as WebUI", () => {
  assert.deepEqual(recommendedExecutionCapabilities(["progress_read", "resource_execution", "duration_report"]), ["resource_execution", "duration_report"])
  assert.equal(executionBlockReason({ assessment_class: "routine", orchestration_state: "discovered", capabilities: ["resource_execution"] }), undefined)
})

test("bot redirects review, formal and private-input work to WebUI", () => {
  assert.match(executionBlockReason({ assessment_class: "formal", orchestration_state: "ready", capabilities: ["resource_execution"] }), /正式测评/)
  assert.match(executionBlockReason({ assessment_class: "routine", orchestration_state: "ready", capabilities: ["submission_execute"] }), /Draft/)
  assert.match(executionBlockReason({ assessment_class: "routine", orchestration_state: "ready", capabilities: ["resource_execution", "question_inventory", "answer_resolve"] }), /审核答案/)
  assert.match(executionBlockReason({ assessment_class: "routine", orchestration_state: "ready", capabilities: ["discussion"] }), /WebUI/)
})
