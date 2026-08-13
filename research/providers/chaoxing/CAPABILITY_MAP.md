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
| ResourceExecution | `Samueli924/chaoxing` | CxKitty | Reference | Document/Read native calls plus signed interval-based Video progress, idempotence and fresh-card verification are offline-covered; Live and Chapter Work remain pending |
| WorkModule TaskInventory | agent skill | OCS, current task pages | PortSource | Course Work list requires a fresh session-bound `enc`; task-page redirect determines submittability |
| ExamModule TaskInventory | agent skill | CxKitty mobile list | PortSource | Browser exam-list route has no `enc`; status text must be parsed after removing scripts |
| TaskDetail | current inventory pipeline | CxKitty, OCS | Reference | Fresh course-bound rediscovery returns the exact Chapter/Resource/Work/Exam task; Work includes followed final-route state, Exam remains list-level until dedicated detail fixtures |
| TaskProgressRead | current inventory and Chapter cards | agent skill, CxKitty | Reference | Resource recovery keeps targeted fresh-card reads; Chapter/Work/Exam use exact fresh Task rediscovery and return conservative state/binary completion, with live fixtures still pending |
| QuestionInventory / QuestionParse | `Samueli924/chaoxing`, OCS current preview pages | CxKitty, `chaoxing-exam` | PortSource / Reference | Independent Work and Chapter Work have offline-covered fresh-page reads with account/correlation/task-bound attempt caches; Chapter Work rebinds all seven cards and its ephemeral `jobid`/`enc`/`ktoken` before one non-replayed attempt GET; Exam remains parse-only and all live behavior is pending |
| SubmissionBuild / Execute | `Samueli924/chaoxing` | CxKitty, OCS, agent skill | Reference | Independent Work rebuilds choice/true-false values from an immutable Draft, reopens one fresh editor, forwards only audited form fields and POSTs `addStudentWorkNew` once; the JSON success flag is only a Receipt, never completion |
| SubmissionVerify | agent skill | `chaoxing-exam`, OCS | PortSource | Independent read-only slot re-discovers the same Work without requiring a Receipt; editor/prompt remain Pending and only a result view whose server-visible per-Question answers exactly match the Draft becomes Confirmed |
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
- Pending Document and Read cards now advertise task-level `ResourceExecution`.
  The execution capability rediscovers the current course/cpi and Chapter card,
  submits only a fresh zeroizing `jtoken`, then refetches all seven cards and
  accepts success only when the remote attachment is completed. Live and
  Chapter Work execution remain current audited implementation gaps; the
  completed native Document/Read/Video slice is not a Provider stopping point.
- Document and Read also advertise `ProgressRead`; the capability performs the
  same bounded course/chapter/card rediscovery without invoking the completion
  endpoint and returns only normalized remote state and percentage. This is the
  safe remote-fact input for crash recovery, not live-account verification.
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
  Work therefore includes the existing followed detail-state check; Exam detail
  remains conservative list evidence until dedicated current fixtures exist.
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
  accepted. Submission and result verification remain active work; Exam still
  requires its distinct attempt-start family and browser gates.
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

## Completion boundary

Chaoxing is complete only when all audited donor abilities—including Chapter
Work, Exam detail/question/answer/submission, Live, QR, captcha/face handling
and required BrowserBridge/Capture fallbacks—have code and all currently
executable verification. A native or parser milestone may be reported but does
not stop development. Immutable Drafts, exact attempt binding, receipt/readback
separation and no ambiguous mutation replay remain mandatory.
