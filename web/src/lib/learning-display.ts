import type { Task } from "@/api/generated/types.gen.ts";

export const providerNames: Record<string, string> = {
  chaoxing: "超星学习通",
  welearn: "WELearn 随行课堂",
  uai: "U校园",
  cidaren: "词达人",
};

export function providerName(providerId: string): string {
  return providerNames[providerId] ?? providerId;
}

export const taskTypeLabels: Record<Task["source_type"], string> = {
  chapter: "章节目录",
  work: "作业",
  exam: "考试",
  resource: "课程资源",
  practice: "练习",
  discussion: "讨论",
  other: "其他任务",
};

export const taskTypeOrder: Task["source_type"][] = [
  "chapter",
  "resource",
  "work",
  "practice",
  "discussion",
  "exam",
  "other",
];

export function remoteStateLabel(state: Task["remote_state"]): string {
  return {
    unknown: "状态未知",
    not_open: "尚未开放",
    pending: "待完成",
    in_progress: "进行中",
    completed: "已完成",
    expired: "已过期",
    removed: "已下架",
  }[state];
}

export function taskActionLabel(task: Task): string {
  if (task.remote_state === "completed") return "查看结果";
  if (task.capabilities.includes("submission_execute")) return "开始作答";
  if (task.capabilities.includes("discussion")) return "填写讨论";
  if (task.capabilities.includes("artifact_upload")) return "上传并完成";
  if (task.capabilities.includes("oral_submission")) return "开始口语任务";
  return "开始执行";
}

export function groupTasks(tasks: readonly Task[]): Array<[Task["source_type"], Task[]]> {
  return taskTypeOrder
    .map((type) => [type, tasks.filter((task) => task.source_type === type)] as [Task["source_type"], Task[]])
    .filter(([, items]) => items.length > 0);
}
