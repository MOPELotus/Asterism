const EXECUTABLE_CAPABILITIES = [
  "resource_execution",
  "submission_execute",
  "duration_report",
  "discussion",
  "artifact_upload",
  "oral_submission",
  "practice",
]

export function recommendedExecutionCapabilities(capabilities) {
  if (capabilities.includes("oral_submission")) return capabilities.includes("submission_execute") ? ["submission_execute", "oral_submission"] : ["oral_submission"]
  if (capabilities.includes("artifact_upload")) return capabilities.includes("submission_execute") ? ["submission_execute", "artifact_upload"] : ["artifact_upload"]
  if (capabilities.includes("discussion")) return ["discussion"]
  if (capabilities.includes("submission_execute")) return ["submission_execute"]
  if (capabilities.includes("resource_execution")) return capabilities.includes("duration_report") ? ["resource_execution", "duration_report"] : ["resource_execution"]
  if (capabilities.includes("duration_report")) return ["duration_report"]
  if (capabilities.includes("practice")) return ["practice"]
  return []
}

export function executionBlockReason(task) {
  const requested = recommendedExecutionCapabilities(task.capabilities || [])
  if (task.assessment_class === "formal") return "正式测评必须在 WebUI 中逐次确认"
  if (!["discovered", "ready", "failed"].includes(task.orchestration_state)) return `当前编排状态 ${task.orchestration_state} 不可直接执行`
  if (requested.length === 0 || !requested.every((value) => EXECUTABLE_CAPABILITIES.includes(value))) return "任务没有可执行能力"
  if (requested.includes("submission_execute")) return "答题提交必须先在 WebUI 审核答案并生成 Draft"
  if (requested.some((value) => ["discussion", "artifact_upload", "oral_submission"].includes(value))) return "任务需要在 WebUI 准备文字、文件或口语输入"
  if ((task.capabilities || []).includes("question_inventory") && (task.capabilities || []).includes("answer_resolve")) return "任务包含题目，必须先在 WebUI 审核答案"
  return undefined
}
