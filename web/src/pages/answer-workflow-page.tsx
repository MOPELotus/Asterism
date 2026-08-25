import { usePermissions } from "@refinedev/core";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { DatabaseZap, FileCheck2, Play, RefreshCw, Sparkles } from "lucide-react";
import { type FormEvent, useEffect, useMemo, useRef, useState } from "react";
import { useNavigate, useParams } from "react-router";

import {
  buildSubmissionDraft,
  createManualAnswerCandidate,
  executeTask,
  generateAiAnswerCandidates,
  getTask,
  getTaskCompletionWorkflows,
  getTaskQuestionSnapshot,
  importLocalAnswerCandidates,
  listAnswerCandidates,
  prepareExecutionInvocationDraft,
  resolveAnswerCandidates,
  resolveProviderAnswerCandidates,
} from "@/api/generated/sdk.gen.ts";
import type { AnswerCandidateResponse, NormalizedAnswer, Question, QuestionGroup, SubmissionDraft } from "@/api/generated/types.gen.ts";
import { requireData } from "@/api/result.ts";
import { PageShell } from "@/components/page-shell.tsx";
import { QueryError, TableSkeleton } from "@/components/query-feedback.tsx";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert.tsx";
import { Badge } from "@/components/ui/badge.tsx";
import { Button } from "@/components/ui/button.tsx";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card.tsx";
import { Input } from "@/components/ui/input.tsx";
import { Label } from "@/components/ui/label.tsx";
import { formatTimestamp, shortId } from "@/lib/format.ts";

export function AnswerWorkflowPage() {
  const { taskId = "", snapshotId = "" } = useParams();
  const queryClient = useQueryClient();
  const permissions = usePermissions<string[]>({});
  const canManageSystem = permissions.data?.includes("manage_system") ?? false;
  const navigate = useNavigate();
  const [selections, setSelections] = useState<Record<string, string>>({});
  const [draft, setDraft] = useState<SubmissionDraft | null>(null);
  const [formalAssessmentConfirmed, setFormalAssessmentConfirmed] = useState(false);
  const [aiProfile, setAiProfile] = useState<"economy" | "gpt_only">("economy");
  const [aiRoute, setAiRoute] = useState<"timed" | "untimed" | "escalation">("untimed");
  const idempotencyKey = useRef(crypto.randomUUID());
  const invocationKey = useRef(crypto.randomUUID());

  const task = useQuery({ queryKey: ["tasks", taskId], enabled: Boolean(taskId), queryFn: async () => requireData(await getTask({ path: { task_id: taskId } })) });
  const completionWorkflows = useQuery({ queryKey: ["tasks", taskId, "completion-workflows"], enabled: Boolean(taskId), retry: false, queryFn: async () => requireData(await getTaskCompletionWorkflows({ path: { task_id: taskId } })) });
  const snapshot = useQuery({ queryKey: ["tasks", taskId, "question-snapshots", snapshotId], enabled: Boolean(taskId && snapshotId), queryFn: async () => requireData(await getTaskQuestionSnapshot({ path: { task_id: taskId, snapshot_id: snapshotId } })) });
  const candidates = useQuery({ queryKey: ["tasks", taskId, "question-snapshots", snapshotId, "candidates"], enabled: Boolean(snapshot.data), queryFn: async () => requireData(await listAnswerCandidates({ path: { task_id: taskId, snapshot_id: snapshotId } })) });
  const resolution = useQuery({ queryKey: ["tasks", taskId, "question-snapshots", snapshotId, "resolution"], enabled: Boolean(snapshot.data && candidates.data), queryFn: async () => requireData(await resolveAnswerCandidates({ path: { task_id: taskId, snapshot_id: snapshotId } })) });

  useEffect(() => {
    if (!resolution.data) return;
    setSelections((current) => {
      const next = { ...current };
      for (const decision of resolution.data.decisions) {
        if (decision.status === "selected" && decision.selected_candidate_id && !next[decision.question_id]) next[decision.question_id] = decision.selected_candidate_id;
      }
      return next;
    });
  }, [resolution.data]);

  async function refreshEvidence() {
    await Promise.all([
      queryClient.invalidateQueries({ queryKey: ["tasks", taskId, "question-snapshots", snapshotId, "candidates"] }),
      queryClient.invalidateQueries({ queryKey: ["tasks", taskId, "question-snapshots", snapshotId, "resolution"] }),
    ]);
    setDraft(null);
  }

  const providerResolve = useMutation({ mutationFn: async () => requireData(await resolveProviderAnswerCandidates({ path: { task_id: taskId, snapshot_id: snapshotId } })), onSuccess: refreshEvidence });
  const localImport = useMutation({ mutationFn: async () => requireData(await importLocalAnswerCandidates({ path: { task_id: taskId, snapshot_id: snapshotId } })), onSuccess: refreshEvidence });
  const autoEvidenceKey = useRef<string | undefined>(undefined);
  useEffect(() => {
    if (!snapshot.data || autoEvidenceKey.current === snapshot.data.snapshot_id) return;
    autoEvidenceKey.current = snapshot.data.snapshot_id;
    // Provider-native evidence and deployment-global cache are the first two
    // answer sources. Both operations are read/idempotent and must happen
    // before presenting the user with an unresolved answer state.
    void providerResolve.mutateAsync(undefined).catch(() => undefined);
    void localImport.mutateAsync(undefined).catch(() => undefined);
  }, [localImport, providerResolve, snapshot.data]);
  const aiGenerate = useMutation({
    mutationFn: async () => {
      if (!snapshot.data) throw new Error("题目快照尚未加载");
      const questionIds = snapshot.data.questions.filter((question) => {
        const decision = resolution.data?.decisions.find((item) => item.question_id === question.id);
        return !decision || decision.status !== "selected";
      }).map((question) => question.id);
      if (!questionIds.length) throw new Error("当前没有缺失或冲突答案需要模型处理");
      return requireData(await generateAiAnswerCandidates({ path: { task_id: taskId, snapshot_id: snapshotId }, body: { profile: aiProfile, route: aiRoute, question_ids: questionIds } }));
    },
    onSuccess: refreshEvidence,
  });
  const buildDraft = useMutation({
    mutationFn: async () => {
      if (!snapshot.data) throw new Error("题目快照尚未加载");
      const answerCandidateIds = snapshot.data.questions.map((question) => selections[question.id]).filter((candidate): candidate is string => Boolean(candidate));
      if (!answerCandidateIds.length) throw new Error("至少选择一个候选答案；最终覆盖率由当前 Provider 运行设置校验");
      return requireData(await buildSubmissionDraft({ path: { task_id: taskId, snapshot_id: snapshotId }, body: { answer_candidate_ids: answerCandidateIds } }));
    },
    onSuccess: setDraft,
  });
  const execute = useMutation({
    mutationFn: async () => {
      if (!draft) throw new Error("请先构建不可变 Submission Draft");
      const strictCompletion = completionWorkflows.data?.strict_completion;
      const strictRetryRequired = strictCompletion?.workflow.state === "active" && strictCompletion.workflow.attempts_started > 0;
      const scoreImprovement = completionWorkflows.data?.score_improvement;
      const scoreImprovementRetakeReady = scoreImprovement?.workflow.state === "ready" && task.data?.orchestration_state === "succeeded" && ["pending", "in_progress"].includes(task.data?.remote_state ?? "");
      return requireData(await executeTask({ path: { task_id: taskId }, headers: { "Idempotency-Key": idempotencyKey.current }, body: { requested_capabilities: ["submission_execute"], submission_draft_id: draft.id, ...(task.data?.assessment_class === "formal" && formalAssessmentConfirmed ? { formal_assessment_confirmation: true } : {}), ...(strictRetryRequired && strictCompletion ? { strict_completion_retry_confirmation: { workflow_id: strictCompletion.workflow.id, expected_revision: strictCompletion.revision } } : {}), ...(scoreImprovementRetakeReady && scoreImprovement ? { score_improvement_retake_confirmation: { workflow_id: scoreImprovement.workflow.id, expected_revision: scoreImprovement.revision } } : {}) } }));
    },
    onSuccess: ({ execution }) => { idempotencyKey.current = crypto.randomUUID(); navigate(`/executions/${execution.id}`); },
  });

  const groupedCandidates = useMemo(() => {
    const result = new Map<string, AnswerCandidateResponse[]>();
    for (const candidate of candidates.data?.candidates ?? []) {
      const group = result.get(candidate.candidate.question_id) ?? [];
      group.push(candidate);
      result.set(candidate.candidate.question_id, group);
    }
    return result;
  }, [candidates.data]);
  const selectedCandidates = useMemo(() => new Map(
    (candidates.data?.candidates ?? []).map((candidate) => [candidate.id, candidate]),
  ), [candidates.data]);
  const applyRecommended = () => {
    if (!snapshot.data) return;
    const next: Record<string, string> = {};
    for (const question of snapshot.data.questions) {
      const decision = resolution.data?.decisions.find((item) => item.question_id === question.id);
      if (decision?.status === "selected" && decision.selected_candidate_id) {
        next[question.id] = decision.selected_candidate_id;
        continue;
      }
      const available = groupedCandidates.get(question.id) ?? [];
      const preferred = [...available].sort(compareCandidates)[0];
      if (preferred) next[question.id] = preferred.id;
    }
    setSelections(next);
    setDraft(null);
  };
  const executeChaoxing = useMutation({
    mutationFn: async (mode: "save" | "submit") => {
      if (!snapshot.data) throw new Error("题目快照尚未加载");
      const answers = snapshot.data.questions.flatMap((question) => {
        const remoteId = question.remote_question_id;
        const selectedId = selections[question.id];
        const candidate = selectedId ? selectedCandidates.get(selectedId) : undefined;
        if (!remoteId || !candidate) return [];
        return [{
          remote_id: remoteId,
          value: chaoxingWorkerAnswer(question, candidate.candidate.answer),
        }];
      });
      if (!answers.length) throw new Error("至少需要选择一道答案；最终提交覆盖率由当前运行设置校验");
      const providerInputType = snapshot.data.provider_id === "cidaren" ? "cidaren.worker.answers.v1" : "chaoxing.worker.answers.v1";
      const invocation = requireData(await prepareExecutionInvocationDraft({
        path: { task_id: taskId },
        headers: {
          "Idempotency-Key": invocationKey.current,
          "x-asterism-invocation-input-type": providerInputType,
          "x-asterism-requested-capabilities": "resource_execution",
        },
        body: new Blob([JSON.stringify({ answers, mode })], { type: "application/octet-stream" }),
      }));
      const result = requireData(await executeTask({
        path: { task_id: taskId },
        headers: { "Idempotency-Key": idempotencyKey.current },
        body: {
          requested_capabilities: ["resource_execution"],
          invocation_draft_id: invocation.draft_id,
          ...(task.data?.assessment_class === "formal" && mode === "save" ? { formal_assessment_save_only: true } : {}),
          ...(task.data?.assessment_class === "formal" && mode === "submit" && formalAssessmentConfirmed ? { formal_assessment_confirmation: true } : {}),
        },
      }));
      return result;
    },
    onSuccess: ({ execution }) => {
      invocationKey.current = crypto.randomUUID();
      idempotencyKey.current = crypto.randomUUID();
      navigate(`/executions/${execution.id}`);
    },
  });

  const error = task.error ?? completionWorkflows.error ?? snapshot.error ?? candidates.error ?? resolution.error ?? providerResolve.error ?? localImport.error ?? aiGenerate.error ?? buildDraft.error ?? execute.error ?? executeChaoxing.error;
  if (snapshot.isLoading || task.isLoading) return <PageShell title="答案审核" description="正在读取不可变题目快照。"><TableSkeleton /></PageShell>;
  if (!snapshot.data || !task.data) return <PageShell title="答案审核" description="快照不存在或不属于当前任务。">{error ? <QueryError error={error} /> : null}</PageShell>;

  const selectedCount = snapshot.data.questions.filter((question) => selections[question.id]).length;
  const complexQuestions = snapshot.data.questions.filter((question) => ["matching", "ordering", "composite", "unknown"].includes(question.kind));
  const canResolveProvider = task.data.capabilities.includes("answer_resolve");
  const canBuild = task.data.capabilities.includes("submission_build");
  const isWorkerAnswerFlow = (snapshot.data.provider_id === "chaoxing" || snapshot.data.provider_id === "cidaren") && task.data.capabilities.includes("resource_execution");
  const strictCompletion = completionWorkflows.data?.strict_completion;
  const strictRetryRequired = strictCompletion?.workflow.state === "active" && strictCompletion.workflow.attempts_started > 0;
  const scoreImprovement = completionWorkflows.data?.score_improvement;
  const scoreImprovementRetakeReady = scoreImprovement?.workflow.state === "ready" && task.data.orchestration_state === "succeeded" && ["pending", "in_progress"].includes(task.data.remote_state);

  return <PageShell title="答案审核" description={`${task.data.title} · snapshot ${shortId(snapshotId)}`}>
    {error ? <QueryError error={error} /> : null}
    <Alert><FileCheck2 className="size-4" /><AlertTitle>不可变审核边界</AlertTitle><AlertDescription>候选来源不会自动成为提交答案。可以明确留下暂未支持或无可靠答案的题目；Core 会按完整快照和 Provider 覆盖率设置决定能否构建 Draft。</AlertDescription></Alert>
    {complexQuestions.length ? <Alert><AlertTitle>发现 {complexQuestions.length} 道复杂或平台原生题</AlertTitle><AlertDescription>连线、排序、共享选项和复合题可像普通题一样选择候选或直接填写。Worker 会优先使用平台原生提交编码；只有页面形态确实无法绑定时才转 BrowserBridge。</AlertDescription></Alert> : null}
    {snapshot.data.groups.length ? <QuestionGroupOverview groups={snapshot.data.groups} questions={snapshot.data.questions} /> : null}
    <Card><CardHeader><div className="flex flex-wrap items-center justify-between gap-3"><div><CardTitle>候选证据</CardTitle><p className="mt-1 text-sm text-muted-foreground">采集于 {formatTimestamp(snapshot.data.captured_at)} · 已选择 {selectedCount}/{snapshot.data.questions.length}</p></div><div className="flex flex-wrap gap-2"><Button variant="outline" disabled={localImport.isPending} onClick={() => localImport.mutate()}><DatabaseZap className="size-4" />导入全局精确缓存</Button>{canResolveProvider ? <Button variant="outline" disabled={providerResolve.isPending} onClick={() => providerResolve.mutate()}><Sparkles className="size-4" />平台标准答案</Button> : null}<Button variant="outline" disabled={!candidates.data?.candidates.length} onClick={applyRecommended}>采用全部推荐答案</Button><Button variant="ghost" onClick={() => void refreshEvidence()}><RefreshCw className="size-4" />刷新</Button></div></div></CardHeader><CardContent><div className="grid gap-3 rounded-lg border p-3 md:grid-cols-[1fr_1fr_auto]"><div className="space-y-1"><Label htmlFor="ai-profile">AI 组合</Label><select id="ai-profile" className="h-9 w-full rounded-md border bg-background px-3 text-sm" value={aiProfile} onChange={(event) => setAiProfile(event.target.value as typeof aiProfile)}><option value="economy">默认省钱组合</option>{canManageSystem ? <option value="gpt_only">GPT-only 保质组合</option> : null}</select></div><div className="space-y-1"><Label htmlFor="ai-route">调用场景</Label><select id="ai-route" className="h-9 w-full rounded-md border bg-background px-3 text-sm" value={aiRoute} onChange={(event) => setAiRoute(event.target.value as typeof aiRoute)}><option value="untimed">不限时题</option><option value="timed">限时题</option>{canManageSystem ? <option value="escalation">难题升级</option> : null}</select></div><Button className="self-end" disabled={aiGenerate.isPending} onClick={() => aiGenerate.mutate()}><Sparkles className="size-4" />{aiGenerate.isPending ? "模型处理中…" : "补全缺失与冲突"}</Button></div></CardContent></Card>

    <div className="space-y-5">{snapshot.data.questions.map((question) => <QuestionReview key={question.id} question={question} candidates={groupedCandidates.get(question.id) ?? []} selected={selections[question.id]} resolutionState={resolution.data?.decisions.find((decision) => decision.question_id === question.id)?.status} onSelect={(candidateId) => { setDraft(null); setSelections((current) => ({ ...current, [question.id]: candidateId })); }} onClear={() => { setDraft(null); setSelections((current) => { const next = { ...current }; delete next[question.id]; return next; }); }} onCreated={refreshEvidence} taskId={taskId} snapshotId={snapshotId} />)}</div>

    <Card><CardHeader><CardTitle>Submission Draft</CardTitle></CardHeader><CardContent className="space-y-4">
      {isWorkerAnswerFlow ? <><Alert><AlertTitle>{snapshot.data.provider_id === "cidaren" ? "Cidaren 上游执行" : "Chaoxing 上游执行"}</AlertTitle><AlertDescription>Asterism 会把当前逐题选中的答案作为加密私有调用输入交回 Worker；独立作业和 Exam 可先保存，最终提交需再次确认。</AlertDescription></Alert>{task.data.assessment_class === "formal" ? <><div className="flex flex-wrap gap-2"><Button variant="outline" disabled={executeChaoxing.isPending || selectedCount === 0} onClick={() => executeChaoxing.mutate("save")}><FileCheck2 className="size-4" />{executeChaoxing.isPending ? "正在处理…" : "仅保存答案"}</Button></div><label className="flex items-start gap-2 rounded-lg border border-amber-300 bg-amber-50 p-3 text-sm dark:border-amber-800 dark:bg-amber-950/30"><input className="mt-1" type="checkbox" checked={formalAssessmentConfirmed} onChange={(event) => setFormalAssessmentConfirmed(event.target.checked)} /><span>我已检查当前答案，确认进行最终提交；该确认仅用于本次请求。</span></label><Button disabled={executeChaoxing.isPending || selectedCount === 0 || !formalAssessmentConfirmed} onClick={() => executeChaoxing.mutate("submit")}><Play className="size-4" />{executeChaoxing.isPending ? "正在处理…" : "确认并最终提交"}</Button></> : <Button disabled={executeChaoxing.isPending || selectedCount === 0} onClick={() => executeChaoxing.mutate("submit")}><Play className="size-4" />{executeChaoxing.isPending ? "正在准备并调度…" : "按已审核答案执行"}</Button>}{selectedCount !== snapshot.data.questions.length ? <p className="text-sm text-muted-foreground">当前已选择 {selectedCount}/{snapshot.data.questions.length}；最终提交会按管理员设置的最小覆盖率校验，缺失题不会随机猜答。</p> : null}</> : null}
      {!draft ? <Button disabled={!canBuild || buildDraft.isPending || selectedCount === 0} onClick={() => buildDraft.mutate()}><FileCheck2 className="size-4" />{buildDraft.isPending ? "构建中…" : "按当前覆盖构建 Draft"}</Button> : <><div className="rounded-lg border p-4"><div className="flex flex-wrap gap-2"><Badge variant="outline">{draft.id}</Badge><Badge variant="secondary">{draft.items.length}/{draft.answer_coverage.total_question_count} 题</Badge><Badge variant="secondary">最低覆盖 {draft.answer_coverage.minimum_coverage_millis / 10}%</Badge><Badge variant="secondary">{draft.payload_preview.encoding}</Badge></div><pre className="mt-3 max-h-64 overflow-auto rounded-md bg-muted p-3 text-xs">{JSON.stringify(draft.payload_preview, null, 2)}</pre></div>{strictRetryRequired ? <Alert><AlertTitle>Strict Completion 重试</AlertTitle><AlertDescription>本次将使用 workflow {shortId(strictCompletion!.workflow.id)} 的当前 revision {strictCompletion!.revision} 明确确认重试；提交仍要求这份新 Draft。</AlertDescription></Alert> : null}{scoreImprovementRetakeReady ? <Alert><AlertTitle>提分重试</AlertTitle><AlertDescription>本次将把这份新 Draft 与 workflow {shortId(scoreImprovement!.workflow.id)} revision {scoreImprovement!.revision} 原子绑定并开始一次重考。</AlertDescription></Alert> : null}{task.data.assessment_class === "formal" ? <Alert className="border-amber-300 bg-amber-50 dark:border-amber-800 dark:bg-amber-950/30"><AlertTitle>正式测评需要本次明确确认</AlertTitle><AlertDescription><label className="mt-2 flex items-start gap-2"><input className="mt-1" type="checkbox" checked={formalAssessmentConfirmed} onChange={(event) => setFormalAssessmentConfirmed(event.target.checked)} /><span>我确认提交这个已审核的不可变 Draft；未勾选时 Core 保持默认拒绝。</span></label></AlertDescription></Alert> : null}<Button disabled={execute.isPending || (task.data.assessment_class === "formal" && !formalAssessmentConfirmed)} onClick={() => execute.mutate()}><Play className="size-4" />{execute.isPending ? "正在调度…" : scoreImprovementRetakeReady ? "确认提分重试并执行" : strictRetryRequired ? "确认重试并执行" : "提交 Draft 并执行"}</Button></>}
      {!canBuild && !isWorkerAnswerFlow ? <p className="text-sm text-muted-foreground">此 Task 未声明 SubmissionBuild capability。</p> : null}
    </CardContent></Card>
  </PageShell>;
}

function chaoxingWorkerAnswer(question: Question, answer: NormalizedAnswer): unknown {
  switch (answer.type) {
    case "selections":
      if (!answer.value.length) throw new Error("选择题答案为空");
      const selections = answer.value.map((value) => chaoxingWorkerOptionValue(question, value));
      return question.kind === "single_choice" ? selections[0] : selections;
    case "texts":
      if (!answer.value.length) throw new Error("文本答案为空");
      return question.kind === "fill_blank" ? answer.value : answer.value.length === 1 ? answer.value[0] : answer.value;
    case "boolean":
      return answer.value;
    case "ordering":
      if (!answer.value.length) throw new Error("排序答案为空");
      return answer.value.map((value) => chaoxingWorkerOptionValue(question, value));
    case "pairs":
      if (!answer.value.length) throw new Error("配对答案为空");
      return Object.fromEntries(answer.value.map((pair) => [chaoxingWorkerOptionValue(question, pair.left), chaoxingWorkerOptionValue(question, pair.right)]));
    case "composite":
      if (!answer.value.length) throw new Error("复合答案为空");
      return answer.value.map((item) => chaoxingWorkerAnswer(question, item));
    case "skip":
    case "unknown":
      throw new Error("未知或跳过答案不能交给 Chaoxing 上游执行");
  }
}

function chaoxingWorkerOptionValue(question: Question, internalId: string): string {
  const option = question.options.find((candidate) => candidate.id === internalId);
  if (!option) return internalId;
  const metadata = option.metadata_sanitized;
  const providerId = metadata && typeof metadata === "object" && !Array.isArray(metadata)
    ? (metadata as Record<string, unknown>).provider_option_id
    : null;
  if (typeof providerId === "string" && providerId.trim()) {
    const occurrences = question.options.filter((candidate) => {
      const candidateMetadata = candidate.metadata_sanitized;
      return candidateMetadata && typeof candidateMetadata === "object" && !Array.isArray(candidateMetadata)
        && (candidateMetadata as Record<string, unknown>).provider_option_id === providerId;
    }).length;
    if (occurrences === 1) return providerId;
  }
  return option.content?.trim() || internalId;
}

function QuestionReview({ question, candidates, selected, resolutionState, onSelect, onClear, onCreated, taskId, snapshotId }: { question: Question; candidates: AnswerCandidateResponse[]; selected?: string; resolutionState?: string; onSelect: (candidateId: string) => void; onClear: () => void; onCreated: () => Promise<void>; taskId: string; snapshotId: string }) {
  return <Card><CardHeader><div className="flex flex-wrap items-center gap-2"><Badge variant="outline">#{question.position}</Badge><Badge variant="secondary">{question.kind}</Badge>{resolutionState ? <Badge variant={resolutionState === "selected" ? "success" : "warning"}>{resolutionState}</Badge> : null}</div><CardTitle className="whitespace-pre-wrap text-base leading-relaxed"><RichQuestionText value={question.stem} /></CardTitle></CardHeader><CardContent className="space-y-3">
    {question.options.length ? <div className="space-y-2 rounded-lg border bg-muted/30 p-3"><p className="text-sm font-medium">题目选项与关系</p>{question.options.map((option) => <div key={option.id} className="rounded-md border bg-background p-3 text-sm"><div className="flex items-start gap-2"><code className="shrink-0 rounded bg-muted px-1.5 py-0.5 text-xs">{option.id}</code><span className="min-w-0 whitespace-pre-wrap"><RichQuestionText value={option.content || "（无文本内容）"} imageRole="选项图片" /></span></div>{option.attachments.length ? <AttachmentPreview attachments={option.attachments} role="选项附件" embeddedContent={option.content} /> : null}{hasVisibleMetadata(option.metadata_sanitized) ? <pre className="mt-2 overflow-auto whitespace-pre-wrap break-words rounded bg-muted p-2 text-xs">{JSON.stringify(option.metadata_sanitized, null, 2)}</pre> : null}</div>)}</div> : null}
    {question.attachments.length ? <AttachmentPreview attachments={question.attachments} role="题干附件" embeddedContent={question.stem} /> : null}
    {candidates.map((candidate) => <label key={candidate.id} className={`block cursor-pointer rounded-lg border p-3 ${selected === candidate.id ? "border-primary bg-primary/5" : ""}`}><div className="flex items-start gap-3"><input className="mt-1" type="radio" name={`answer-${question.id}`} checked={selected === candidate.id} onChange={() => onSelect(candidate.id)} /><div className="min-w-0 flex-1"><div className="flex flex-wrap gap-2"><Badge variant="outline">{answerSourceLabel(candidate.candidate.source)}</Badge>{candidate.candidate.confidence == null ? null : <Badge variant="secondary">置信度 {candidate.candidate.confidence / 100}%</Badge>}<span className="font-mono text-xs text-muted-foreground">{shortId(candidate.id)}</span></div><AnswerPreview answer={candidate.candidate.answer} question={question} />{candidate.candidate.explanation ? <p className="mt-2 text-sm text-muted-foreground">{candidate.candidate.explanation}</p> : null}</div></div></label>)}
    {!candidates.length ? <p className="text-sm text-muted-foreground">尚无候选证据。</p> : null}
    <div className="flex flex-wrap gap-2">{selected ? <Button size="sm" variant="outline" onClick={onClear}>不提交此题</Button> : null}<ManualCandidateForm question={question} taskId={taskId} snapshotId={snapshotId} onCreated={onCreated} /></div>
  </CardContent></Card>;
}

function hasVisibleMetadata(value: unknown) {
  if (value == null) return false;
  if (Array.isArray(value)) return value.length > 0;
  if (typeof value === "object") return Object.keys(value).length > 0;
  return true;
}

function RichQuestionText({ value, imageRole = "题干图片" }: { value: string; imageRole?: string }) {
  const tokens = value.split(/(\[UNDERLINE\][\s\S]*?\[\/UNDERLINE\]|\[BLANK_\d+\]|\[QUESTION_(?:IMAGE|AUDIO|VIDEO|FILE|FORMULA):[^\]]+\])/g);
  return <>{tokens.map((token, index) => {
    const underlined = token.match(/^\[UNDERLINE\]([\s\S]*)\[\/UNDERLINE\]$/);
    if (underlined) return <u key={index} className="decoration-2 underline-offset-2">{underlined[1]}</u>;
    if (/^\[BLANK_\d+\]$/.test(token)) return <span key={index} className="mx-1 inline-block min-w-20 border-b-2 border-current text-center text-xs text-muted-foreground">{token.slice(1, -1)}</span>;
    const image = token.match(/^\[QUESTION_IMAGE:([^\]]+)\]$/);
    const imageUrl = image?.[1];
    if (imageUrl) return isSafeDisplayUrl(imageUrl) ? <img key={index} className="my-2 inline-block max-h-80 max-w-full rounded border object-contain align-middle" src={imageUrl} alt={imageRole} loading="lazy" referrerPolicy="no-referrer" /> : <Badge key={index} variant="outline">{imageRole}</Badge>;
    const media = token.match(/^\[QUESTION_(AUDIO|VIDEO):([^\]]+)\]$/);
    if (media) {
      const url = media[2] ?? "";
      if (!isSafeDisplayUrl(url)) return <Badge key={index} variant="outline">{media[1] === "AUDIO" ? "音频" : "视频"}</Badge>;
      return media[1] === "AUDIO"
        ? <audio key={index} className="my-2 max-w-full align-middle" src={url} controls preload="none" />
        : <video key={index} className="my-2 inline-block max-h-80 max-w-full rounded border align-middle" src={url} controls preload="none" />;
    }
    const file = token.match(/^\[QUESTION_FILE:([^\]|]+)(?:\|([^\]]+))?\]$/);
    if (file) {
      const fileUrl = file[1] ?? "";
      return isSafeDisplayUrl(fileUrl) ? <a key={index} className="mx-1 underline underline-offset-2" href={fileUrl} target="_blank" rel="noreferrer">{file[2] || "题目附件"}</a> : <Badge key={index} variant="outline">{file[2] || "题目附件"}</Badge>;
    }
    const formula = token.match(/^\[QUESTION_FORMULA:([^\]]+)\]$/);
    if (formula) return <code key={index} className="mx-1 rounded bg-muted px-1.5 py-0.5 font-mono text-sm">{formula[1] === "embedded" ? "公式" : formula[1]}</code>;
    return token;
  })}</>;
}

function isSafeDisplayUrl(value: string) {
  try {
    const url = new URL(value);
    return url.protocol === "https:" || url.protocol === "http:";
  } catch {
    return false;
  }
}

function AttachmentPreview({ attachments, role, embeddedContent }: { attachments: Question["attachments"]; role: string; embeddedContent?: string | null }) {
  const remaining = attachments.filter((attachment) => !attachment.remote_id || !embeddedContent?.includes(attachment.remote_id));
  if (!remaining.length) return null;
  return <div className="mt-2 flex flex-wrap gap-2">{remaining.map((attachment, index) => attachment.kind === "image" && attachment.remote_id && isSafeDisplayUrl(attachment.remote_id) ? <img key={`${attachment.kind}-${attachment.remote_id}`} className="max-h-64 max-w-full rounded border object-contain" src={attachment.remote_id} alt={attachment.label || role} loading="lazy" referrerPolicy="no-referrer" /> : <Badge key={`${attachment.kind}-${attachment.remote_id ?? index}`} variant="outline">{attachment.kind}{attachment.label ? ` · ${attachment.label}` : ""}</Badge>)}</div>;
}

function QuestionGroupOverview({ groups, questions }: { groups: QuestionGroup[]; questions: Question[] }) {
  const groupById = new Map(groups.map((group) => [group.id, group]));
  const questionById = new Map(questions.map((question) => [question.id, question]));
  const childGroupIds = new Set(groups.flatMap((group) => group.children.filter((child) => child.type === "group").map((child) => child.id)));
  const roots = groups.filter((group) => !childGroupIds.has(group.id));
  const renderGroup = (group: QuestionGroup, depth: number): React.ReactNode => <div key={group.id} className="space-y-2 rounded-lg border p-3" style={{ marginLeft: `${Math.min(depth, 4) * 12}px` }}>
    <div className="flex flex-wrap gap-2"><Badge variant="outline">题组</Badge>{group.remote_group_id ? <Badge variant="secondary">{group.remote_group_id}</Badge> : null}<Badge variant="secondary">{group.children.length} 个子项</Badge></div>
    {group.stem ? <p className="whitespace-pre-wrap text-sm"><RichQuestionText value={group.stem} /></p> : null}
    {group.options.length ? <div className="grid gap-1 text-sm text-muted-foreground">{group.options.map((option) => <div key={option.id} className="flex items-start gap-2"><span className="font-mono">{option.id}</span>{option.content ? <span><RichQuestionText value={option.content} imageRole="共享选项图片" /></span> : null}</div>)}</div> : null}
    {group.attachments.length ? <AttachmentPreview attachments={group.attachments} role="共享材料附件" embeddedContent={group.stem} /> : null}
    <div className="space-y-2">{group.children.map((child) => child.type === "group" ? (groupById.get(child.id) ? renderGroup(groupById.get(child.id)!, depth + 1) : <p key={child.id} className="text-sm text-destructive">缺失子题组 {child.id}</p>) : <p key={child.id} className="text-sm text-muted-foreground">题目 #{(questionById.get(child.id)?.position ?? -1) + 1} · {questionById.get(child.id)?.stem ?? child.id}</p>)}</div>
  </div>;
  return <Card><CardHeader><CardTitle>共享材料与复合题结构</CardTitle></CardHeader><CardContent className="space-y-3">{roots.map((group) => renderGroup(group, 0))}</CardContent></Card>;
}

function ManualCandidateForm({ question, taskId, snapshotId, onCreated }: { question: Question; taskId: string; snapshotId: string; onCreated: () => Promise<void> }) {
  const [open, setOpen] = useState(false);
  const [answer, setAnswer] = useState<NormalizedAnswer>(() => initialAnswer(question));
  const [confidence, setConfidence] = useState("");
  const [explanation, setExplanation] = useState("");
  const [parseError, setParseError] = useState<string | null>(null);
  const create = useMutation({ mutationFn: async () => {
    validateManualAnswer(answer);
    return requireData(await createManualAnswerCandidate({ path: { task_id: taskId, snapshot_id: snapshotId }, body: { question_id: question.id, answer, ...(confidence ? { confidence_basis_points: Number(confidence) } : {}), ...(explanation.trim() ? { explanation: explanation.trim() } : {}) } }));
  }, onSuccess: async () => { setAnswer(initialAnswer(question)); setConfidence(""); setExplanation(""); setParseError(null); setOpen(false); await onCreated(); }, onError: (error) => setParseError(error instanceof Error ? error.message : "创建候选失败") });
  if (!open) return <Button size="sm" variant="ghost" onClick={() => setOpen(true)}>添加人工候选</Button>;
  return <form className="space-y-3 rounded-lg border border-dashed p-3" onSubmit={(event: FormEvent) => { event.preventDefault(); setParseError(null); create.mutate(); }}><VisualAnswerEditor question={question} answer={answer} onChange={setAnswer} /><div className="grid gap-3 sm:grid-cols-2"><div className="space-y-2"><Label htmlFor={`confidence-${question.id}`}>置信度（0–100%，可选）</Label><Input id={`confidence-${question.id}`} type="number" min={0} max={100} value={confidence ? String(Number(confidence) / 100) : ""} onChange={(event) => setConfidence(event.target.value ? String(Math.round(Number(event.target.value) * 100)) : "")} /></div><div className="space-y-2"><Label htmlFor={`explanation-${question.id}`}>说明（可选）</Label><Input id={`explanation-${question.id}`} value={explanation} onChange={(event) => setExplanation(event.target.value)} /></div></div>{parseError ? <p className="text-sm text-destructive">{parseError}</p> : null}<div className="flex gap-2"><Button size="sm" type="submit" disabled={create.isPending}>{create.isPending ? "保存中" : "保存并选用"}</Button><Button size="sm" variant="ghost" type="button" onClick={() => setOpen(false)}>取消</Button></div></form>;
}

function answerSourceLabel(source: AnswerCandidateResponse["candidate"]["source"]): string {
  return ({ manual: "人工补漏", local_cache: "全局精确缓存", provider_native: "平台标准答案", ai: "AI", external_bank: "外部题库", other: "其他" } as const)[source] ?? source;
}

function compareCandidates(left: AnswerCandidateResponse, right: AnswerCandidateResponse): number {
  const rank = { provider_native: 0, local_cache: 1, manual: 2, ai: 3, external_bank: 4, other: 5 } as const;
  const sourceDelta = (rank[left.candidate.source] ?? 99) - (rank[right.candidate.source] ?? 99);
  if (sourceDelta) return sourceDelta;
  return (right.candidate.confidence ?? -1) - (left.candidate.confidence ?? -1);
}

function initialAnswer(question: Question): NormalizedAnswer {
  switch (question.kind) {
    case "single_choice":
    case "multiple_choice": return { type: "selections", value: [] };
    case "true_false": return { type: "boolean", value: true };
    case "fill_blank":
    case "short_answer": return { type: "texts", value: [""] };
    case "matching": return { type: "pairs", value: [] };
    case "ordering": return { type: "ordering", value: [] };
    case "composite": return { type: "composite", value: [] };
    case "unknown": return { type: "unknown" };
  }
}

function validateManualAnswer(answer: NormalizedAnswer): void {
  if ("value" in answer && Array.isArray(answer.value) && answer.value.length === 0) throw new Error("答案不能为空");
  if (answer.type === "texts" && answer.value.some((value) => !value.trim())) throw new Error("文本答案不能为空");
  if (answer.type === "skip" || answer.type === "unknown") throw new Error("请填写可提交的答案");
}

function AnswerPreview({ answer, question }: { answer: NormalizedAnswer; question: Question }) {
  const optionText = new Map(question.options.map((option) => [option.id, option.content]));
  let value: string;
  switch (answer.type) {
    case "selections": value = answer.value.map((id) => `${id}${optionText.get(id) ? ` · ${optionText.get(id)}` : ""}`).join("；"); break;
    case "texts": value = answer.value.join(" / "); break;
    case "boolean": value = answer.value ? "正确" : "错误"; break;
    case "ordering": value = answer.value.join(" → "); break;
    case "pairs": value = answer.value.map((pair) => `${pair.left} → ${pair.right}`).join("；"); break;
    case "composite": value = answer.value.map((item) => JSON.stringify(item)).join("；"); break;
    case "skip": value = "跳过"; break;
    case "unknown": value = "未知"; break;
  }
  return <p className="mt-2 whitespace-pre-wrap break-words rounded bg-muted p-2 text-sm">{value || "（空）"}</p>;
}

function VisualAnswerEditor({ question, answer, onChange }: { question: Question; answer: NormalizedAnswer; onChange: (answer: NormalizedAnswer) => void }) {
  if (question.kind === "single_choice" || question.kind === "multiple_choice") {
    const selected = answer.type === "selections" ? answer.value : [];
    return <fieldset className="space-y-2"><Label>选择答案</Label>{question.options.map((option) => <label key={option.id} className="flex items-start gap-2 rounded border p-2 text-sm"><input type={question.kind === "single_choice" ? "radio" : "checkbox"} name={`manual-${question.id}`} checked={selected.includes(option.id)} onChange={(event) => { const value = question.kind === "single_choice" ? [option.id] : event.target.checked ? [...selected, option.id] : selected.filter((id) => id !== option.id); onChange({ type: "selections", value }); }} /><span>{option.id}{option.content ? ` · ${option.content}` : ""}</span></label>)}</fieldset>;
  }
  if (question.kind === "true_false") {
    const value = answer.type === "boolean" ? answer.value : true;
    return <div className="space-y-2"><Label>判断答案</Label><div className="flex gap-4"><label><input type="radio" checked={value} onChange={() => onChange({ type: "boolean", value: true })} /> 正确</label><label><input type="radio" checked={!value} onChange={() => onChange({ type: "boolean", value: false })} /> 错误</label></div></div>;
  }
  const serialized = JSON.stringify(answer, null, 2);
  return <div className="space-y-2"><Label htmlFor={`manual-answer-${question.id}`}>{question.kind === "fill_blank" || question.kind === "short_answer" ? "答案（多个空用换行分隔）" : "结构化答案"}</Label>{question.kind === "fill_blank" || question.kind === "short_answer" ? <textarea id={`manual-answer-${question.id}`} className="min-h-24 w-full rounded-md border bg-background p-2 text-sm" value={answer.type === "texts" ? answer.value.join("\n") : ""} onChange={(event) => onChange({ type: "texts", value: event.target.value.split("\n") })} /> : <textarea id={`manual-answer-${question.id}`} className="min-h-32 w-full rounded-md border bg-background p-2 font-mono text-xs" value={serialized} onChange={(event) => { try { onChange(JSON.parse(event.target.value) as NormalizedAnswer); } catch { /* Keep the last valid structured value while typing. */ } }} />}</div>;
}
