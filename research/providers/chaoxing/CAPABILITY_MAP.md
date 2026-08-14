# chaoxing capability map

`ChapterModule`, independent `WorkModule` and `ExamModule` remain separate
source modules even when their assessments share field names or HTML shapes.

| Asterism capability | Primary source | Secondary source | Planned use | Current evidence |
|---|---|---|---|---|
| Authentication / Password | `Samueli924/chaoxing` | CxKitty | PortSource | AES-CBC `fanyalogin`, derived Composite credential and Cookie revalidation are offline-covered; live validation pending |
| Authentication / QR | CxKitty | BrowserBridge capture | Reference | QR creation and polling chain is statically complete but old; current browser/session fallback is required first-batch work |
| Session persistence and expiry | `Samueli924/chaoxing` | CxKitty | Reference | Cookie import plus `_uid`/SSO or course-list validation |
| CourseInventory | `Samueli924/chaoxing` | CxKitty | PortSource | Web `courselistdata`, interaction folder discovery and merge are offline-covered; live session validation remains pending |
| ChapterModule inventory | `Samueli924/chaoxing` | CxKitty | Reference | Native chapter tree and bounded 0-6 card inventory are offline-covered; live pending |
| ResourceExecution | `Samueli924/chaoxing` | CxKitty | Reference | Document/Read native calls, signed interval-based Video progress and Live `liveinfo` -> durable `saveTimePc` heartbeats -> fresh ProgressRead verification are offline-covered; every Live heartbeat is issue/receipt ledgered and ambiguous outcomes are never replayed |
| WorkModule TaskInventory | agent skill | OCS, current task pages | PortSource | Course Work list requires a fresh session-bound `enc`; task-page redirect determines submittability |
| ExamModule TaskInventory | agent skill | CxKitty mobile list | PortSource | Browser exam-list route has no `enc`; status text is parsed after removing scripts, while bounded score and structural `reTest(...)` availability remain read-only facts |
| TaskDetail | current inventory pipeline | CxKitty, OCS | Reference | Fresh course-bound rediscovery returns the exact Chapter/Resource/Work/Exam task; Work includes followed final-route state, while completed Exams with a strictly bound preview entry add fresh result score/retake provenance without enabling retake execution |
| TaskProgressRead | current inventory and Chapter cards | agent skill, CxKitty | Reference | Document/Read/Video/Live resource recovery keeps targeted fresh-card reads; Chapter/Work/Exam use exact fresh Task rediscovery and return conservative state/binary completion, with live-account validation still pending |
| QuestionInventory / QuestionParse | `Samueli924/chaoxing`, OCS current preview pages | CxKitty, `chaoxing-exam` | PortSource / Reference | Independent Work and Chapter Work have offline-covered fresh-page reads with account/correlation/task-bound attempt caches; Chapter Work rebinds all seven cards and its ephemeral `jobid`/`enc`/`ktoken` before one non-replayed attempt GET; pending Exam tasks use cover -> one-shot start -> full attempt-bound mobile preview and retain only the rotated bounded attempt state, while exam-code/face/captcha gates return typed BrowserRequired |
| AnswerResolve | `chaoxing-exam` completed Chapter result | Samueli Tiku, CxKitty searchers, OCS wrappers/cache, agent pre-computed answers | Reference | A typed but unregistered component rebinds fresh completed Chapter Work detail and consumes one abstract bound result document before producing strict ProviderNative candidates; all other donor sources are external/manual/random and no pending Work/Exam standard-answer protocol exists |
| SubmissionBuild / Execute | `Samueli924/chaoxing` | CxKitty, OCS, agent skill | Reference | Independent Work and Chapter Work rebuild answers from immutable Drafts. Exam has a separate value-free preview and durable one-operation-at-a-time native chain: donor signature and request digest are frozen before dispatch, each accepted answer save advances one cursor and rotates `enc`/timing state, and the final submit yields only a Receipt. No Exam mutation is routed through Work payloads or replayed after ambiguity |
| SubmissionVerify | agent skill, `Samueli924/chaoxing` | `chaoxing-exam`, OCS | PortSource / Reference | Independent Work verifies exact server-visible answers; Chapter Work refreshes seven cards and has strict current verification / standard-answer parsers awaiting its BrowserBridge iframe route; Exam uses Completed only for task recovery, confirms Questions solely from exact Draft ID/order/type/value result evidence, and projects the independent bounded result score as fixed thousandths |
| Error classification | CxKitty | agent skill, `Samueli924/chaoxing` | Reference | Auth, captcha, face, timing, access, protocol and network branches exist upstream |
| BrowserBridge / Capture | agent skill | OCS, `chaoxing-exam`, CxKitty | Reference | Current first-batch fallback for QR/session binding, captcha/face gates and any donor capability Native HTTP cannot express reliably |

## Required Asterism separations

```text
ChapterModule
  - Video
  - Document
  - Read
  - Chapter Work

WorkModule
  - independent course homework

ExamModule
  - independent course exams
```

`ExamModule` is a source classification. It does not set
`assessment_class = Formal`; that value must come from explicit Provider facts or
policy and remains independently guarded.

## Implementation order

1. Add sanitized HTML fixture contracts for Work and Exam lists.
2. Implement deterministic Work/Exam inventory parsers and state mapping.
3. Implement the authenticated transport with Native HTTP first.
4. Implement bounded BrowserBridge/Capture recipes for QR, session binding and
   the donor-observed captcha/face/browser gates; live-test which routes require
   them when accounts become available.
5. Route only the affected capability through BrowserBridge when Native HTTP is
   incomplete or unreliable; do not move unrelated capabilities into a browser
   runtime.
6. Add detail, question, submission, and remote-result verification only after
   inventory identity and status are stable.

## Current implementation checkpoint

- The `courselistdata` parser models `courseId + clazzId` as stable identity,
  filters unopened rows, validates the allowlisted course route, and keeps `cpi`
  only in ephemeral route context. A typed `CourseInventoryCapability` merges
  identical folder results and fails closed on conflicts.
- Password and ImportedCookie now compose behind `AuthenticationCapability`.
  Password derives username/password/Cookie as a Core-revalidated Composite;
  captcha and SMS outcomes stop with typed `HumanRequired` reasons.
- The Native HTTP adapter fetches the root list, discovers bounded folder IDs
  from the interaction page, and fetches every folder using one short-lived
  resolved session. Any request or parse failure aborts the entire inventory.
- Independent Work and Exam list parsers are covered by synthetic sanitized
  fixtures and compose behind `TaskInventoryCapability`.
- Exam rows retain an optional 0-100 score and structural `reTest(...)`
  availability. Minute text, malformed precision/range, conflicting scores and
  lookalike handlers are regression-covered; neither fact changes state or
  capabilities, and no retake command is implemented by this checkpoint.
- Completed Exam rows with the audited preview path now perform one bounded
  same-origin result read bound to course/class/exam. Missing, duplicate,
  foreign or conflicting detail results abort the atomic inventory; accepted
  facts update only normalized score/retake provenance.
- Course/class identity and `cpi` cross the course-to-task boundary only through
  the non-serialized ephemeral route context.
- The Native HTTP adapter uses the shared NetworkProfile, an injected session
  resolver, strict redirect/error classification, and structural discovery of a
  fresh Work `enc` route. Real-account behavior remains unverified.
- Native HTTP versus BrowserBridge remains a per-capability live-test decision;
  the presence of this adapter is not evidence that the `uf` session works in
  reqwest.
- Chaoxing stays at `Development`; the daemon keeps it absent by default and
  registers it only through the explicit local-validation opt-in.
- Pending Document, Read and Live cards advertise task-level `ResourceExecution`.
  The immediate Document/Read path rediscovers the current course/cpi and
  Chapter card, submits only a fresh zeroizing `jtoken`, then refetches all seven
  cards and accepts success only when the remote attachment is completed. Live
  uses the separately ledgered duration/heartbeat path below. Chapter Work
  execution remains on its immutable-Draft path; no resource kind is routed
  through the wrong mutation family.
- Document, Read, Video and Live advertise `ProgressRead`; the capability performs the
  same bounded course/chapter/card rediscovery without invoking the completion
  endpoint and returns only normalized remote state and percentage. This is the
  safe remote-fact input for crash recovery, not live-account verification.
- Live execution resolves fresh card-only `liveId`, `streamName`, `vdoid` and
  optional status job material, reads a strict bounded `liveinfo` duration, and
  freezes the status `_uid` across the run. Every `saveTimePc` heartbeat records
  its provider-namespaced ordinal and exact semantic request digest before send,
  then records the exact response digest and accepted bit before the next
  ordinal. Any ambiguous send, parse or receipt failure becomes HumanRequired
  and is not renewed or replayed. The final proof is a separate fresh
  `ProgressRead` whose exact resource must report Completed; the fixture does
  not claim live-account validation.
- Video execution now re-resolves fresh non-persisted Card metadata, obtains one
  bounded status/dtoken response, reports monotonic intervals with the donor
  signature, and accepts completion only after a fresh Card reports passed.
  Playback rate and report interval come from the immutable Master-controlled
  Execution settings snapshot. The native boundary stops captcha responses as
  typed `HumanRequired` before blind retry; the audited solver and bounded
  Capture/BrowserBridge fallback remain active first-batch work.
- Task detail now parses the stable remote identity, rediscovers the exact
  current course and runs one complete fresh Task inventory for that course.
  Missing courses/tasks become `RemoteChanged`, duplicate identities fail as
  protocol drift, and the returned normalized detail contains no route context.
  Work therefore includes the existing followed detail-state check. A supported
  completed Exam preview returns freshly rebound list state plus bounded result
  score/retake facts; other entry/detail variants remain pending fixtures.
- Module-aware progress now keeps executable Resource recovery on its targeted
  fresh-card path while Chapter, independent Work and Exam reuse exact Task
  rediscovery. Completed/Pending become conservative 100/0 binary completion;
  other states expose no invented percentage. These tasks advertise
  `ProgressRead`, but the result remains offline fixture evidence only.
- Independent Work advertises `QuestionInventory` and `QuestionParse` only after
  its fresh detail redirect ends at the readable editor. The Question reader
  rediscovers the course and Work entry, accepts only the final `/work/dowork`
  route, parses one bounded page and shares it through a five-minute
  account/correlation/task-bound process-local cache. HTML, route credentials
  and attempt-local QIDs are never persisted.
- Pending Chapter Work now advertises the same two read capabilities without
  being collapsed into independent Work. It rediscovers the exact Chapter,
  requires a complete unique seven-card set, extracts one matching fresh
  `jobid`/`enc`/`ktoken`, and sends the donor `api/work` GET once. Because that
  GET can create the attempt, authentication is resolved before the request and
  no ambiguous failure is renewed or replayed. Only the audited same-host
  `doHomeWorkNew` redirect and exact course/class/knowledge/job binding are
  accepted. Exam retains its distinct attempt-start/submission family and
  browser gates.
- The same fresh pending independent Work state now advertises
  `SubmissionBuild`, `SubmissionExecute` and `SubmissionVerify` together for
  the native subset. Build remains value-free. Execute currently accepts only
  single-choice, multiple-choice and true/false Questions with unambiguous
  donor encodings, rechecks the complete immutable Draft and fresh Work route,
  extracts only an allowlisted bounded form from the editor and performs one
  POST without mutation retry. Verify is a separate read-only rediscovery and
  can recover without a Receipt; prompt/editor pages never claim success, and
  a result view must expose the exact submitted answer for every Draft
  Question. This is synthetic/offline evidence only and does not raise the
  Provider above Development.
- Pending Chapter Work also advertises `SubmissionBuild`, `SubmissionExecute`
  and `SubmissionVerify`. Build supports donor type codes 0-4; Execute rechecks
  the immutable Draft and fresh Course/Chapter/seven-card target, then sends one
  attempt GET and one final POST without ambiguous replay. Verify ignores the
  Receipt as proof and confirms only a fresh exact card with `isPassed=true`;
  per-Question facts stay Unverified because a completion card is not answer
  readback. A separate Provider parser now accepts only the donor-observed
  `selectWorkQuestionYiPiYue` result shape: complete unique `singleQuesId`
  identity/order/type facts plus visible single/multiple/true-false answers are
  compared per Question, while the independent 0-100 final score is retained as
  exact thousandths. Missing, extra, reordered, fill/short-answer or otherwise
  unsupported facts are Inconclusive, and duplicate identities or malformed or
  conflicting scores fail closed. This parser is not yet wired into the
  capability because the audited result is reached through a study-page iframe
  whose shared BrowserBridge runtime remains Main-owned.
- The same bound Chapter Work result parser can map a complete `正确答案` set to
  `ProviderNative` AnswerCandidates for single-choice, multiple-choice and
  true/false Questions. Candidate creation rechecks QID/order/type and current
  option IDs, uses full confidence only for the server-declared standard value,
  and retains sanitized route/QID provenance without copying result HTML.
  Missing answers, fill/short-answer types, reordered Questions or unknown
  options fail closed. `AnswerResolve` is not advertised and no `redoTest`
  mutation is implemented until the shared BrowserBridge supplies the fully
  bound fresh result document and Main wires the capability lifecycle.
- `ChaoxingAnswerResolve` now implements the typed Provider API around that
  parser without changing development metadata, the Provider factory or Task
  capability advertisement. It first requires an authenticated context and a
  fresh exact completed Chapter Work TaskDetail whose normalized
  course/class/knowledge/job facts match the stable remote identity. Only then
  may `ChaoxingAnswerResolutionTransport` return one bounded result document.
  Pending tasks and foreign page kinds fail before transport. Registration is
  blocked on the Main-owned BrowserBridge iframe read and on keeping this
  post-result evidence distinct from pre-submission answer resolution.
- Pending independent Exam rows with a structural `goTest` entry now advertise
  `QuestionInventory` and `QuestionParse`. The fresh read rebinds the exact
  course/class/exam identity and `enc_task`, obtains the donor cover, freezes a
  versioned exact `phone/start` command and hands it to Core's durable
  pre-Question attempt ledger. Before sending, the Provider repeats the
  read-only discovery and requires the encrypted command digest to remain
  identical. Core records the operation type/request digest first; the Native
  transport then sends it once, validates the resulting `examAnswerId` and
  dynamic `enc`, and fetches the complete bounded mobile preview. The preview's
  newly rotated `enc` and timing fields replace start-page state. The normalized
  Question set and a minimal encrypted attempt artifact are materialized
  atomically; neither HTML nor answer content is persisted. Ambiguous start
  errors remain locked rather than replayed. Exam-code, face and captcha gates
  return typed `HumanRequired` pending their BrowserBridge/Capture command path.
- Pending Exam tasks now also advertise `SubmissionExecute` and
  `SubmissionVerify`. The immutable Draft and encrypted v2 attempt artifact bind
  the complete ordered Question set and exact save cursor. The Provider freezes
  CxKitty-compatible `pos/rd/value/_edt` signature parameters and the complete
  form/query digest before Core marks each operation issued. Accepted temporary
  saves must return monotonic `lastUpdate|encRemain|enc` state and atomically
  rotate the continuation; after every Question is saved, one separate final
  submit returns a Receipt. Fresh exact Exam inventory must report Completed
  before result verification. Per-Question confirmation then requires a fresh
  bound preview result whose complete IDs, DOM order, `exam_mobile` types and
  visible values match the immutable Draft. Missing/extra/unsupported evidence
  is Inconclusive, duplicates fail closed, and only a fully bound value mismatch
  is Rejected. The same fresh result's optional 0-100 score is carried as exact
  thousandths on any verification status but never participates in the answer
  decision. An ambiguous temporary save stays locked, while an ambiguous final
  submit may be recovered only from a fresh Completed list state.

## Completion boundary

Chaoxing is complete only when all audited donor abilities—including remaining
Chapter Work result-route/answer-resolution/retake wiring, Exam detail/result
variants, Live account validation, QR, captcha/face handling
and required BrowserBridge/Capture fallbacks—have code and all currently
executable verification. A native or parser milestone may be reported but does
not stop development. Immutable Drafts, exact attempt binding, receipt/readback
separation and no ambiguous mutation replay remain mandatory.

## 2026-08-14 full-sweep additions

The one-time full donor re-audit is recorded in `FULL_UPSTREAM_SWEEP.md`. It
adds the following accepted boundaries without weakening existing capability
contracts:

- partial-answer submission is in scope with the audited Samueli default
  `cover_rate=0.9`, but Main must bind the resolved policy, complete snapshot
  denominator and intentional unanswered set into the immutable Draft before
  Chaoxing can execute a subset;
- result harvesting feeds an owner-level Answer Evidence Corpus only from a
  read-only, strictly rebound result route; task completion, score, `我的答案`,
  `正确答案` and per-Question correctness remain separate facts;
- first account binding defaults to bounded read-only historical bootstrap, and
  normal `SubmissionVerify` contributes incremental evidence, but neither
  lifecycle exists in the shared capability/storage model yet;
- Strict Completion and Score Improvement are independent default-enabled
  state machines. A score-improvement retake needs an explicit capability,
  fresh snapshot/Draft, bounded attempt authority and exact result-proven
  eligibility; `retake_available` alone never authorizes mutation;
- OCS-observed types 11, 14 and 15 and shared-option issue #297 establish the
  matching/child/shared-context boundary. The current parser recognizes the
  families, while BrowserBridge mutation and shared child modeling remain
  required for lossless completion;
- Audio, sign-in, post-task learning-count, in-video Questions and hyperlink
  execution remain evidenced donor capabilities requiring their own fixtures,
  durable mutation design and verification rather than deletion from scope.
