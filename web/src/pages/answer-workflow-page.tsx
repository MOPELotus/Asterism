import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { DatabaseZap, FileCheck2, Play, RefreshCw, Sparkles } from "lucide-react";
import { type FormEvent, useEffect, useMemo, useRef, useState } from "react";
import { useNavigate, useParams } from "react-router";

import {
  buildSubmissionDraft,
  createManualAnswerCandidate,
  executeTask,
  getTask,
  getTaskQuestionSnapshot,
  importLocalAnswerCandidates,
  listAnswerCandidates,
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
  const navigate = useNavigate();
  const [selections, setSelections] = useState<Record<string, string>>({});
  const [draft, setDraft] = useState<SubmissionDraft | null>(null);
  const idempotencyKey = useRef(crypto.randomUUID());

  const task = useQuery({ queryKey: ["tasks", taskId], enabled: Boolean(taskId), queryFn: async () => requireData(await getTask({ path: { task_id: taskId } })) });
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
      return requireData(await executeTask({ path: { task_id: taskId }, headers: { "Idempotency-Key": idempotencyKey.current }, body: { requested_capabilities: ["submission_execute"], submission_draft_id: draft.id } }));
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

  const error = task.error ?? snapshot.error ?? candidates.error ?? resolution.error ?? providerResolve.error ?? localImport.error ?? buildDraft.error ?? execute.error;
  if (snapshot.isLoading || task.isLoading) return <PageShell title="答案审核" description="正在读取不可变题目快照。"><TableSkeleton /></PageShell>;
  if (!snapshot.data || !task.data) return <PageShell title="答案审核" description="快照不存在或不属于当前任务。">{error ? <QueryError error={error} /> : null}</PageShell>;

  const selectedCount = snapshot.data.questions.filter((question) => selections[question.id]).length;
  const canResolveProvider = task.data.capabilities.includes("answer_resolve");
  const canBuild = task.data.capabilities.includes("submission_build");

  return <PageShell title="答案审核" description={`${task.data.title} · snapshot ${shortId(snapshotId)}`}>
    {error ? <QueryError error={error} /> : null}
    <Alert><FileCheck2 className="size-4" /><AlertTitle>不可变审核边界</AlertTitle><AlertDescription>候选来源不会自动成为提交答案。可以明确留下暂未支持或无可靠答案的题目；Core 会按完整快照和 Provider 覆盖率设置决定能否构建 Draft。</AlertDescription></Alert>
    {snapshot.data.groups.length ? <QuestionGroupOverview groups={snapshot.data.groups} questions={snapshot.data.questions} /> : null}
    <Card><CardHeader className="flex-row items-center justify-between"><div><CardTitle>候选证据</CardTitle><p className="mt-1 text-sm text-muted-foreground">采集于 {formatTimestamp(snapshot.data.captured_at)} · 已选择 {selectedCount}/{snapshot.data.questions.length}</p></div><div className="flex flex-wrap gap-2"><Button variant="outline" disabled={localImport.isPending} onClick={() => localImport.mutate()}><DatabaseZap className="size-4" />导入本地证据</Button>{canResolveProvider ? <Button variant="outline" disabled={providerResolve.isPending} onClick={() => providerResolve.mutate()}><Sparkles className="size-4" />Provider 解析</Button> : null}<Button variant="ghost" onClick={() => void refreshEvidence()}><RefreshCw className="size-4" />刷新</Button></div></CardHeader></Card>

    <div className="space-y-5">{snapshot.data.questions.map((question) => <QuestionReview key={question.id} question={question} candidates={groupedCandidates.get(question.id) ?? []} selected={selections[question.id]} resolutionState={resolution.data?.decisions.find((decision) => decision.question_id === question.id)?.status} onSelect={(candidateId) => { setDraft(null); setSelections((current) => ({ ...current, [question.id]: candidateId })); }} onClear={() => { setDraft(null); setSelections((current) => { const next = { ...current }; delete next[question.id]; return next; }); }} onCreated={refreshEvidence} taskId={taskId} snapshotId={snapshotId} />)}</div>

    <Card><CardHeader><CardTitle>Submission Draft</CardTitle></CardHeader><CardContent className="space-y-4">
      {!draft ? <Button disabled={!canBuild || buildDraft.isPending || selectedCount === 0} onClick={() => buildDraft.mutate()}><FileCheck2 className="size-4" />{buildDraft.isPending ? "构建中…" : "按当前覆盖构建 Draft"}</Button> : <><div className="rounded-lg border p-4"><div className="flex flex-wrap gap-2"><Badge variant="outline">{draft.id}</Badge><Badge variant="secondary">{draft.items.length}/{draft.answer_coverage.total_question_count} 题</Badge><Badge variant="secondary">最低覆盖 {draft.answer_coverage.minimum_coverage_millis / 10}%</Badge><Badge variant="secondary">{draft.payload_preview.encoding}</Badge></div><pre className="mt-3 max-h-64 overflow-auto rounded-md bg-muted p-3 text-xs">{JSON.stringify(draft.payload_preview, null, 2)}</pre></div>{task.data.assessment_class === "routine" ? <Button disabled={execute.isPending} onClick={() => execute.mutate()}><Play className="size-4" />{execute.isPending ? "正在调度…" : "提交 Draft 并执行"}</Button> : <Alert className="border-amber-300 bg-amber-50"><AlertTitle>正式任务仍被 Core 策略阻止</AlertTitle><AlertDescription>需要持久化审批契约后才能执行；WebUI 不会用本地确认绕过。</AlertDescription></Alert>}</>}
      {!canBuild ? <p className="text-sm text-muted-foreground">此 Task 未声明 SubmissionBuild capability。</p> : null}
    </CardContent></Card>
  </PageShell>;
}

function QuestionReview({ question, candidates, selected, resolutionState, onSelect, onClear, onCreated, taskId, snapshotId }: { question: Question; candidates: AnswerCandidateResponse[]; selected?: string; resolutionState?: string; onSelect: (candidateId: string) => void; onClear: () => void; onCreated: () => Promise<void>; taskId: string; snapshotId: string }) {
  return <Card><CardHeader><div className="flex flex-wrap items-center gap-2"><Badge variant="outline">#{question.position + 1}</Badge><Badge variant="secondary">{question.kind}</Badge>{resolutionState ? <Badge variant={resolutionState === "selected" ? "success" : "warning"}>{resolutionState}</Badge> : null}</div><CardTitle className="whitespace-pre-wrap text-base leading-relaxed">{question.stem}</CardTitle></CardHeader><CardContent className="space-y-3">
    {candidates.map((candidate) => <label key={candidate.id} className={`block cursor-pointer rounded-lg border p-3 ${selected === candidate.id ? "border-primary bg-primary/5" : ""}`}><div className="flex items-start gap-3"><input className="mt-1" type="radio" name={`answer-${question.id}`} checked={selected === candidate.id} onChange={() => onSelect(candidate.id)} /><div className="min-w-0 flex-1"><div className="flex flex-wrap gap-2"><Badge variant="outline">{candidate.candidate.source}</Badge>{candidate.candidate.confidence == null ? null : <Badge variant="secondary">置信度 {candidate.candidate.confidence}</Badge>}<span className="font-mono text-xs text-muted-foreground">{shortId(candidate.id)}</span></div><pre className="mt-2 overflow-auto whitespace-pre-wrap break-words rounded bg-muted p-2 text-xs">{JSON.stringify(candidate.candidate.answer, null, 2)}</pre>{candidate.candidate.explanation ? <p className="mt-2 text-sm text-muted-foreground">{candidate.candidate.explanation}</p> : null}</div></div></label>)}
    {!candidates.length ? <p className="text-sm text-muted-foreground">尚无候选证据。</p> : null}
    <div className="flex flex-wrap gap-2">{selected ? <Button size="sm" variant="outline" onClick={onClear}>不提交此题</Button> : null}<ManualCandidateForm questionId={question.id} taskId={taskId} snapshotId={snapshotId} onCreated={onCreated} /></div>
  </CardContent></Card>;
}

function QuestionGroupOverview({ groups, questions }: { groups: QuestionGroup[]; questions: Question[] }) {
  const groupById = new Map(groups.map((group) => [group.id, group]));
  const questionById = new Map(questions.map((question) => [question.id, question]));
  const childGroupIds = new Set(groups.flatMap((group) => group.children.filter((child) => child.type === "group").map((child) => child.id)));
  const roots = groups.filter((group) => !childGroupIds.has(group.id));
  const renderGroup = (group: QuestionGroup, depth: number): React.ReactNode => <div key={group.id} className="space-y-2 rounded-lg border p-3" style={{ marginLeft: `${Math.min(depth, 4) * 12}px` }}>
    <div className="flex flex-wrap gap-2"><Badge variant="outline">题组</Badge>{group.remote_group_id ? <Badge variant="secondary">{group.remote_group_id}</Badge> : null}<Badge variant="secondary">{group.children.length} 个子项</Badge></div>
    {group.stem ? <p className="whitespace-pre-wrap text-sm">{group.stem}</p> : null}
    {group.options.length ? <div className="grid gap-1 text-sm text-muted-foreground">{group.options.map((option) => <div key={option.id}><span className="font-mono">{option.id}</span>{option.content ? ` · ${option.content}` : ""}</div>)}</div> : null}
    {group.attachments.length ? <div className="flex flex-wrap gap-2">{group.attachments.map((attachment, index) => <Badge key={`${attachment.kind}-${attachment.remote_id ?? index}`} variant="outline">{attachment.kind}{attachment.label ? ` · ${attachment.label}` : ""}</Badge>)}</div> : null}
    <div className="space-y-2">{group.children.map((child) => child.type === "group" ? (groupById.get(child.id) ? renderGroup(groupById.get(child.id)!, depth + 1) : <p key={child.id} className="text-sm text-destructive">缺失子题组 {child.id}</p>) : <p key={child.id} className="text-sm text-muted-foreground">题目 #{(questionById.get(child.id)?.position ?? -1) + 1} · {questionById.get(child.id)?.stem ?? child.id}</p>)}</div>
  </div>;
  return <Card><CardHeader><CardTitle>共享材料与复合题结构</CardTitle></CardHeader><CardContent className="space-y-3">{roots.map((group) => renderGroup(group, 0))}</CardContent></Card>;
}

function ManualCandidateForm({ questionId, taskId, snapshotId, onCreated }: { questionId: string; taskId: string; snapshotId: string; onCreated: () => Promise<void> }) {
  const [open, setOpen] = useState(false);
  const [answerJson, setAnswerJson] = useState("");
  const [confidence, setConfidence] = useState("");
  const [explanation, setExplanation] = useState("");
  const [parseError, setParseError] = useState<string | null>(null);
  const create = useMutation({ mutationFn: async () => {
    let answer: NormalizedAnswer;
    try { answer = JSON.parse(answerJson) as NormalizedAnswer; } catch { throw new Error("答案必须是合法的 NormalizedAnswer JSON"); }
    return requireData(await createManualAnswerCandidate({ path: { task_id: taskId, snapshot_id: snapshotId }, body: { question_id: questionId, answer, ...(confidence ? { confidence_basis_points: Number(confidence) } : {}), ...(explanation.trim() ? { explanation: explanation.trim() } : {}) } }));
  }, onSuccess: async () => { setAnswerJson(""); setConfidence(""); setExplanation(""); setParseError(null); setOpen(false); await onCreated(); }, onError: (error) => setParseError(error instanceof Error ? error.message : "创建候选失败") });
  if (!open) return <Button size="sm" variant="ghost" onClick={() => setOpen(true)}>添加人工候选</Button>;
  return <form className="space-y-3 rounded-lg border border-dashed p-3" onSubmit={(event: FormEvent) => { event.preventDefault(); setParseError(null); create.mutate(); }}><div className="space-y-2"><Label htmlFor={`answer-json-${questionId}`}>NormalizedAnswer JSON</Label><Input id={`answer-json-${questionId}`} required value={answerJson} onChange={(event) => setAnswerJson(event.target.value)} placeholder={'{"type":"boolean","value":true}'} /></div><div className="grid gap-3 sm:grid-cols-2"><div className="space-y-2"><Label htmlFor={`confidence-${questionId}`}>置信度 basis points（可选）</Label><Input id={`confidence-${questionId}`} type="number" min={0} max={10000} value={confidence} onChange={(event) => setConfidence(event.target.value)} /></div><div className="space-y-2"><Label htmlFor={`explanation-${questionId}`}>说明（可选）</Label><Input id={`explanation-${questionId}`} value={explanation} onChange={(event) => setExplanation(event.target.value)} /></div></div>{parseError ? <p className="text-sm text-destructive">{parseError}</p> : null}<div className="flex gap-2"><Button size="sm" type="submit" disabled={create.isPending}>{create.isPending ? "保存中" : "保存候选"}</Button><Button size="sm" variant="ghost" onClick={() => setOpen(false)}>取消</Button></div></form>;
}
