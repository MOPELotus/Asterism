# chaoxing capability map

The first implementation target remains independent `WorkModule` and
`ExamModule` inventory. `ChapterModule` is a separate source module even when a
Chapter card contains a Work-shaped assessment.

| Asterism capability | Primary source | Secondary source | Planned use | Current evidence |
|---|---|---|---|---|
| Authentication / Password | `Samueli924/chaoxing` | CxKitty | PortSource | AES-CBC `fanyalogin`, derived Composite credential and Cookie revalidation are offline-covered; live validation pending |
| Authentication / QR | CxKitty | None | Reference | QR creation and polling chain is statically complete but old |
| Session persistence and expiry | `Samueli924/chaoxing` | CxKitty | Reference | Cookie import plus `_uid`/SSO or course-list validation |
| CourseInventory | `Samueli924/chaoxing` | CxKitty | PortSource | Web `courselistdata`, interaction folder discovery and merge are offline-covered; live session validation remains pending |
| ChapterModule inventory | `Samueli924/chaoxing` | CxKitty | Reference | Chapter tree, task-point counts, and card attachments |
| ResourceExecution | `Samueli924/chaoxing` | CxKitty | Reference | Video, Document, Read, and existing Chapter Work behavior |
| WorkModule TaskInventory | agent skill | OCS, current task pages | PortSource | Course Work list requires a fresh session-bound `enc`; task-page redirect determines submittability |
| ExamModule TaskInventory | agent skill | CxKitty mobile list | PortSource | Browser exam-list route has no `enc`; status text must be parsed after removing scripts |
| TaskDetail | agent skill | CxKitty, OCS | Reference | Work final URL and Exam entry/detail pages carry task-specific state |
| TaskProgressRead | agent skill | CxKitty | Reference | Submitted/detail routes and score/result pages; normalized progress still needs fixtures |
| QuestionInventory / QuestionParse | CxKitty | OCS, `chaoxing-exam` | Reference | Question IDs, types, options, hidden answers, and per-attempt QID changes |
| SubmissionBuild / Execute | CxKitty | OCS, agent skill | Reference | Native/mobile form and Browser event paths differ; formal-assessment guard remains Core-owned |
| SubmissionVerify | agent skill | `chaoxing-exam` | PortSource | Re-fetch final task page, verify server-visible answers/result rather than HTTP 200 or CSS |
| Error classification | CxKitty | agent skill, `Samueli924/chaoxing` | Reference | Auth, captcha, face, timing, access, protocol and network branches exist upstream |
| BrowserBridge | agent skill | OCS, `chaoxing-exam` | Reference | Needed if 2026 session binding prevents reliable native HTTP |

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
4. Live-test whether Cookie requests satisfy current `uf` and fingerprint rules.
5. Route only the affected capability through BrowserBridge if Native HTTP is
   unreliable; do not move the entire Provider into a browser runtime.
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
- Chaoxing stays at `Development` and is not registered as an available
  Provider.
