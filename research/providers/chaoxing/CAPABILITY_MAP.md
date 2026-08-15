# chaoxing capability map

`ChapterModule`, independent `WorkModule` and `ExamModule` remain separate
source modules even when their assessments share field names or HTML shapes.

| Asterism capability | Primary source | Secondary source | Planned use | Current evidence |
|---|---|---|---|---|
| Authentication / Password | `Samueli924/chaoxing` | CxKitty | PortSource | AES-CBC `fanyalogin`, derived Composite credential and Cookie revalidation are offline-covered; live validation pending |
| Authentication / QR | CxKitty | BrowserBridge capture | Reference | The Development factory advertises `QrCode` through Core's encrypted CAS continuation lifecycle. Exact Web `uuid`/`enc`, `createqr`, authorized `toauthlogin` action, one-request `getauthstatus`, scoped zeroizing Cookies, typed wait/reject/expire outcomes and fresh Course-list success validation are offline-covered. `status=true` first persists an identity-validation continuation; a later read-only Course-list poll persists the terminal continuation and deterministically finalizes one Cookie bundle. Live validation and BrowserBridge fallback remain open |
| Session persistence and expiry | `Samueli924/chaoxing` | CxKitty | Reference | Cookie import plus `_uid`/SSO or course-list validation |
| CourseInventory | `Samueli924/chaoxing` | CxKitty | PortSource | Web `courselistdata`, interaction folder discovery and merge are offline-covered; live session validation remains pending |
| ChapterModule inventory | `Samueli924/chaoxing` | CxKitty | Reference | Native chapter tree and bounded 0-6 card inventory are offline-covered; live pending |
| ResourceExecution | `Samueli924/chaoxing` | OCS, CxKitty | Reference | Document/Read native calls, structurally distinguished signed interval-based Video/Audio progress and Live `liveinfo` -> durable `saveTimePc` heartbeats -> fresh ProgressRead verification are offline-covered; every Live heartbeat is issue/receipt ledgered and ambiguous outcomes are never replayed |
| WorkModule TaskInventory | agent skill | OCS, current task pages | PortSource | Course Work list requires a fresh session-bound `enc`; task-page redirect determines submittability |
| ExamModule TaskInventory | agent skill | CxKitty mobile list | PortSource | Browser exam-list route has no `enc`; status text is parsed after removing scripts, while bounded score and structural `reTest(...)` availability remain read-only facts |
| TaskDetail | current inventory pipeline | CxKitty, OCS | Reference | Fresh course-bound rediscovery returns the exact Chapter/Resource/Work/Exam task; Work includes followed final-route state, while completed Exams with a strictly bound preview entry add fresh result score/retake provenance without enabling retake execution |
| TaskProgressRead | current inventory and Chapter cards | agent skill, CxKitty | Reference | Document/Read/Video/Audio/Live resource recovery keeps targeted fresh-card reads; Chapter/Work/Exam use exact fresh Task rediscovery and return conservative state/binary completion, with live-account validation still pending |
| QuestionInventory / QuestionParse | `Samueli924/chaoxing`, OCS current preview pages | CxKitty, `chaoxing-exam` | PortSource / Reference | Independent Work and Chapter Work have offline-covered fresh-page reads with account/correlation/task-bound attempt caches; Chapter Work rebinds all seven cards and its ephemeral `jobid`/`enc`/`ktoken` before one non-replayed attempt GET; pending Exam tasks use cover -> one-shot start -> full attempt-bound mobile preview and retain only the rotated bounded attempt state, while exam-code/face/captcha gates return typed BrowserRequired |
| AnswerResolve | `chaoxing-exam` completed Chapter result | Samueli Tiku, CxKitty searchers, OCS wrappers/cache, agent pre-computed answers | Reference | A typed but unregistered component rebinds fresh completed Chapter Work detail and consumes one abstract bound result document before producing strict ProviderNative candidates for choice/true-false and exactly one bound fill blank; all other donor sources are external/manual/random and no pending Work/Exam standard-answer protocol exists |
| AnswerHistoryHarvest | `chaoxing-exam` completed Chapter result | Samueli issue #607, OCS result cache | Reference | A typed capability pages only completed Chapter Work references supplied by a bounded read-only transport, binds the audited `workAnswerId` into a Provider attempt digest, hashes the exact result document, applies Core's TaskId to parsed Questions and emits submitted/official/correctness/score/structural `RedoTest` facts for choice/true-false and exact single-blank text. The native development factory does not advertise it until a real BrowserBridge/Capture list/read transport exists |
| SubmissionBuild / Execute | `Samueli924/chaoxing` | CxKitty, OCS, agent skill | Reference | Independent Work, Chapter Work and Exam execute Core-frozen partial coverage without inventing answers. Work forms preserve the complete current control order and rebuild unanswered values empty. Chapter/Exam fill mutation requires a parsed `blank_count=1` and exactly one Draft text; ambiguous multi-blank concatenation fails closed. Exam's encrypted v3 continuation retains the complete ordered Question identity/fingerprint set plus the frozen selected original positions; saves traverse only selected Draft Questions and send each donor `start` as its original zero-based paper index. No Exam mutation is routed through Work payloads or replayed after ambiguity |
| SubmissionVerify | agent skill, `Samueli924/chaoxing` | `chaoxing-exam`, OCS | PortSource / Reference | Independent Work verifies exact server-visible answers; Chapter Work refreshes seven cards and has strict current verification / standard-answer parsers awaiting its BrowserBridge iframe route; Exam uses Completed only for task recovery, requires the full result Question count and unique identities, and confirms/rejects only selected Draft Questions at their original positions from exact type/value evidence. Exact single-blank submitted text is supported only when the Question binds one blank; pending placeholders are not evidence. Unselected or unsupported result Questions are not answer evidence. The independent bounded result score remains fixed thousandths. A fresh explicit Expired state maps to `WindowClosed`; all less specific incomplete states remain conservatively undiagnosed |
| Error classification | CxKitty | agent skill, `Samueli924/chaoxing` | Reference | Auth, captcha, face, timing, access, protocol and network branches exist upstream. Unknown numeric Question kinds, unknown Chapter resource structures and unknown Work result shapes now attach bounded protocol observations containing only controlled enums, counts and field-presence facts |
| BrowserBridge / Capture | agent skill | OCS, `chaoxing-exam`, CxKitty | Reference | Fallback for a live-drifted QR/session route, captcha/face gates and any donor capability Native HTTP cannot express reliably |

## Required Asterism separations

```text
ChapterModule
  - Video / Audio
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
4. Implement bounded BrowserBridge/Capture recipes for any live-drifted QR
   route and the donor-observed captcha/face/browser gates; live-test which
   routes require them when accounts become available.
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
- QR now composes behind `AuthenticationCapability` and the Development factory
  advertises `QrCode`. A canonical `chaoxing.qr.v1` secret continuation binds
  ProviderAccount/AuthSession/correlation, exact challenge material, scoped
  Cookies, absolute expiry, poll count and phase. Core encrypts it, claims one
  poll revision at a time and atomically rotates waiting states. Provider
  revision is modeled separately from Core's consumed poll sequence, so a
  retryable released claim may advance the bounded sequence without invalidating
  the unchanged encrypted continuation. Scanned
  identity text is ignored, response bodies and Cookie values are zeroized,
  and host-only Passport Cookies never cross to Course hosts. `status=true`
  first rotates to a durable identity-validation phase after checking that the
  resulting Cookie exposes `_uid`/`UID`; the next poll performs only a fresh
  Course-list read. A transient validation failure can therefore retry that
  read without replaying `getauthstatus`. Only a successful read persists the
  authenticated terminal continuation used for deterministic Cookie-bundle
  finalization without another remote poll.
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
- Pending Document, Read, Video, Audio and Live cards advertise task-level
  `ResourceExecution`.
  The immediate Document/Read path rediscovers the current course/cpi and
  Chapter card, submits only a fresh zeroizing `jtoken`, then refetches all seven
  cards and accepts success only when the remote attachment is completed. Live
  uses the separately ledgered duration/heartbeat path below. Chapter Work
  execution remains on its immutable-Draft path; no resource kind is routed
  through the wrong mutation family.
- Document, Read, Video, Audio and Live advertise `ProgressRead`; the capability
  performs the same bounded course/chapter/card rediscovery without invoking the
  completion endpoint and returns only normalized remote state and percentage.
  This is the safe remote-fact input for crash recovery, not live-account
  verification.
- Live execution resolves fresh card-only `liveId`, `streamName`, `vdoid` and
  optional status job material, reads a strict bounded `liveinfo` duration, and
  freezes the status `_uid` across the run. Every `saveTimePc` heartbeat records
  its provider-namespaced ordinal and exact semantic request digest before send,
  then records the exact response digest and accepted bit before the next
  ordinal. Any ambiguous send, parse or receipt failure becomes HumanRequired
  and is not renewed or replayed. The final proof is a separate fresh
  `ProgressRead` whose exact resource must report Completed; the fixture does
  not claim live-account validation.
- Video/Audio execution now re-resolves fresh non-persisted Card metadata,
  obtains one bounded status/dtoken response, reports monotonic intervals with
  the donor signature, and accepts completion only after a fresh Card reports
  passed. Audio is selected only when the current OCS-observed
  `property.module` is exactly `insertaudio`; its request uses the audited Audio
  Referer and `dtype=Audio`. A failed Video request is never retried as Audio.
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
- Core-frozen partial coverage is now executable for independent Work, Chapter
  Work and mobile Exam. The short-lived Provider plan retains selected answers plus the
  immutable complete count; the fresh editor must expose that exact count and
  every selected QID/type. `answerwqbid` and `answertype*` preserve the complete
  current DOM order, while unanswered `answer*` values are rebuilt as empty and
  every stale editor answer is discarded. Independent Work result verification
  requires the same complete count and original position for each selected QID,
  then confirms or rejects only Draft Questions. The encrypted mobile Exam v3
  artifact retains the complete ordered QID/position/content-fingerprint set,
  freezes the selected original positions on first Draft binding, advances only
  that selected cursor and emits each save's original zero-based `start` value.
  Result verification likewise requires the full remote count but reads visible
  answer evidence only for selected Questions.
- Pending Chapter Work also advertises `SubmissionBuild`, `SubmissionExecute`
  and `SubmissionVerify`. Build supports donor type codes 0-4; Execute rechecks
  the immutable Draft and fresh Course/Chapter/seven-card target, then sends one
  attempt GET and one final POST without ambiguous replay. Verify ignores the
  Receipt as proof and confirms only a fresh exact card with `isPassed=true`;
  per-Question facts stay Unverified because a completion card is not answer
  readback. A separate Provider parser now accepts only the donor-observed
  `selectWorkQuestionYiPiYue` result shape: complete unique `singleQuesId`
  identity/order/type facts plus visible single/multiple/true-false or exact
  single-blank answers are compared per Question, while the independent 0-100
  final score is retained as
  exact thousandths. Missing, extra, reordered, multi-blank, pending-text,
  short-answer or otherwise unsupported facts are Inconclusive, and duplicate
  identities or malformed or
  conflicting scores fail closed. This parser is not yet wired into the
  capability because the audited result is reached through a study-page iframe
  whose shared BrowserBridge runtime remains Main-owned.
- The same bound Chapter Work result parser can map a complete `正确答案` set to
  `ProviderNative` AnswerCandidates for single-choice, multiple-choice,
  true/false and exactly one bound fill blank. Candidate creation rechecks
  QID/order/type, current option IDs where applicable and `blank_count=1`, uses
  full confidence only for the server-declared standard value,
  and retains sanitized route/QID provenance without copying result HTML.
  Missing/pending answers, multi-blank/short-answer types, reordered Questions
  or unknown options fail closed. `AnswerResolve` is not advertised and no `redoTest`
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
  `SubmissionVerify`. The immutable Draft and encrypted v3 attempt artifact bind
  the complete ordered Question set, selected original positions and exact save
  cursor. The Provider freezes
  CxKitty-compatible `pos/rd/value/_edt` signature parameters and the complete
  form/query digest before Core marks each operation issued. Accepted temporary
  saves must return monotonic `lastUpdate|encRemain|enc` state and atomically
  rotate the continuation; after every selected Draft Question is saved, one separate final
  submit returns a Receipt. Fresh exact Exam inventory must report Completed
  before result verification. Per-Question confirmation then requires a fresh
  bound preview result matching every v3 full-paper QID/original-position binding;
  each selected Draft ID must retain its original DOM position and exact
  `exam_mobile` type/visible value. Missing/extra/drifted selected evidence is
  Inconclusive, duplicates fail closed, and only a fully bound selected value
  mismatch is Rejected. Unselected unsupported types need no visible answer and
  are never promoted to answer evidence. The same fresh result's optional 0-100 score is carried as exact
  thousandths on any verification status but never participates in the answer
  decision. An ambiguous temporary save stays locked, while an ambiguous final
  submit may be recovered only from a fresh Completed list state.

## Completion boundary

Chaoxing is complete only when all audited donor abilities—including remaining
Chapter Work result-route/answer-resolution/retake wiring, Exam detail/result
variants, Live/QR account validation, captcha/face handling
and required BrowserBridge/Capture fallbacks—have code and all currently
executable verification. A native or parser milestone may be reported but does
not stop development. Immutable Drafts, exact attempt binding, receipt/readback
separation and no ambiguous mutation replay remain mandatory.

## 2026-08-14 full-sweep additions

The one-time full donor re-audit is recorded in `FULL_UPSTREAM_SWEEP.md`. It
adds the following accepted boundaries without weakening existing capability
contracts:

- partial-answer submission is in scope with the audited Samueli default
  `cover_rate=0.9`. Core now binds the resolved policy, complete snapshot
  denominator and intentional unanswered set into the immutable Draft;
  independent Work and Chapter Work execute that partition through a fresh
  complete editor. Mobile Exam remains a separate request family and now
  executes the same partition through a lossless encrypted v3
  selected-original-position binding;
- result harvesting feeds an owner-level Answer Evidence Corpus only from a
  read-only, strictly rebound result route; task completion, score, `我的答案`,
  `正确答案` and per-Question correctness remain separate facts;
- first account binding defaults to bounded read-only historical bootstrap, and
  normal `SubmissionVerify` contributes incremental evidence. Core now owns the
  harvest/evidence lifecycle; Chaoxing still needs a verified live history
  transport and incremental verification-ingestion wiring;
- a Provider-local Chapter history parser and typed `AnswerHistoryHarvest`
  implementation now close the supported result-DOM
  fact boundary: it keeps submitted and official answers, their exact
  per-Question equality judgement, fixed-point score and structural `redoTest`
  entry separate. The capability binds Core's TaskId plus separate attempt and
  result digests, but remains absent from the native factory pending a real
  read-only list/read transport and never authorizes retake;
- Strict Completion and Score Improvement are independent default-enabled
  state machines. A score-improvement retake needs an explicit capability,
  fresh snapshot/Draft, bounded attempt authority and exact result-proven
  eligibility; `retake_available` alone never authorizes mutation;
- OCS-observed types 11, 14 and 15 and shared-option issue #297 establish the
  matching/child/shared-context boundary. The current parser recognizes the
  families, while BrowserBridge mutation and shared child modeling remain
  required for lossless completion;
- Sign-in, post-task learning-count, in-video Questions and hyperlink execution
  remain evidenced donor protocol surfaces requiring their own fixtures,
  durable mutation design and verification rather than deletion from scope.
  The current sign-in CLI flag is unwired; learning-count advances only a local
  counter without server readback; OCS hyperlink/timed-reader flows prove only
  Browser actions and waits; and the observed in-video strategy is random or
  ignore. None is advertised until exact identity, receipt and independent
  verification are available.
