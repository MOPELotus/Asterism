# Cidaren fixture plan

Repository tests contact no real account. Initial fixtures are synthetic
reconstructions of fields read by frozen donor code and corroborated by the
public issue tracker. A separately supplied redacted live-safe capture proves
the current V2 login envelope/cookieless exchange but is not committed as a
credential fixture.

## Initial synthetic fixtures

```text
fixtures/providers/cidaren/
  auth/token-success.json
  auth/token-rejected.json
  tasks/class-task-page-1.json
  tasks/class-task-page-2.json
  tasks/study-task-list.json
  answers/course-page.json
  answers/study-task-info.json
  answers/study-word-info-envelope.json
  answers/study-word-info-means.json
  questions/start-answer-envelope.json
  questions/start-answer-single.json
```

The class fixture includes multiple Courses, learning and test rows, pending,
in-progress, completed and expired states, `task_id=-1`, page boundaries and
unknown fields that must be dropped. The study fixture adds selected-Course
binding, ordinary `task_type=3` units, stable `list_id`, access flags and
repeated uninitialized task identity behavior.

These fixtures now drive the Development crate's strict token-response,
Course merging, stable release/list identity, state classification, redaction,
all-or-nothing pagination, selected-Course query binding, fresh TaskDetail and
TaskProgressRead tests. They remain synthetic and do not establish live
compatibility; native HTTP request and response boundaries are covered
separately with deterministic transports.

Capture tests require the same-snapshot token and `CDR_LOGIN_INFO` only.
Missing optional donor-observed `CDR_USER_SESSION` must not stall the helper or
expand stored secret material.

Native OAuth tests generate both P-256 peers in memory, DER/SPKI round-trip the
client public key, reproduce the server ECDH/HKDF/AES-GCM response and tamper
the ciphertext/version to prove fail-closed behavior. OAuth code, private key,
shared secret, salt, login plaintext and request/response bodies remain
bounded, redacted where printable and zeroized where owned. No real callback
code or first-party ciphertext is copied into a fixture.

Assisted-bootstrap tests generate independent synthetic state/marker bytes,
freeze the exact WeChat URL shape and `authorize != "2"`, retain only
domain-separated digests outside the short-lived URL, and accept only exact
Cidaren HTTPS callbacks in audited query or SPA-fragment forms. Wrong origins,
userinfo, explicit ports, paths, duplicate fields and mismatched bindings fail
closed; no live OAuth URL or callback is committed.

Inline negative tests must cover empty/oversized/non-JSON responses, non-object
records, unsupported task types/status values, duplicate release identities,
conflicting duplicate Course titles, inconsistent totals, missing pages,
oversized page/row counts, invalid percentages and unsafe remote components.
Legacy decoder tests generate synthetic JSON/base64 and insert the exact
versioned confusion bytes in memory. The synthetic StartAnswer fixture freezes
only the donor-read topic/stem/option shape; no published or real word/question
payload is copied into the repository.

## Required live-sanitized fixtures

After WebUI and Asterism-Plugin/Yunzai are complete and the user supplies an
account, read-only fixture capture covers inventory first; mutation fixtures
require separate explicit authorization:

```text
fixtures/providers/cidaren/
  auth/token-success-live.json
  auth/token-expired-live.json
  courses/class-task-empty-live.json
  tasks/class-task-mixed-live.json
  tasks/class-task-task-id-minus-one-live.json
  tasks/study-task-selected-live.json
  tasks/study-task-task-id-minus-one-live.json
  progress/class-task-fresh-live.json
  progress/study-task-fresh-live.json
  capture/composite-shape-live.json
  questions/start-answer-shape-live.json
  submissions/verify-receipt-live.json
  submissions/fresh-verification-live.json
```

Remove UserToken, browser crypto context, student name/code, school/class,
teacher identifiers, request signatures, exact timestamps, free-form personal
task names and all answer/topic material. Preserve bounded structural fields,
placeholder identities, result codes, status values and pagination shape.

## Regression requirements

- Imported tokens never appear in Debug, fixture, normalized metadata or logs.
- Native V2 login codes are bounded single-use secrets; request signing omits
  interceptor-added `app_type`, response handshake/version/AAD are exact, and
  fresh `Student/Main` must succeed before Core may persist the replacement.
- Assisted OAuth state and marker are independent CSPRNG values, marker is
  never `"2"`, only their domain-separated hashes cross the durable boundary,
  callback code is extracted only after strict origin/path/duplicate/binding
  validation, and the verified bootstrap sends `Authorization-v: 00` plus a
  sensitive empty `UserToken` rather than inheriting an old account token.
  The structured shared challenge preserves those two digests, imported-token
  challenges contain no OAuth artifact, and fresh account readback keeps the
  exact current OAuth version/User-Agent/`Abc`/`Authorization-v` context.
  Native OAuth Composite replacements validate only as
  `NativeProviderLogin`, and the account-bound stored-session resolver accepts
  the same atomic two-field acquisition without mixing origins or timestamps.
  Core's post-exchange validation repeats fresh `Student/Main` with the same
  OAuth context, while Capture-origin validation keeps its donor header family.
- Class-task pagination is complete and total-consistent before normalization.
- Course identity is `course:{course_id}` and duplicate rows must agree.
- Task identity is `class-task:{release_id}` even when `task_id == -1`.
- Ordinary study identity is `study-task:{course_id}:{list_id}` even when
  `task_id == -1`.
- Selected Course, query and `StudyTask/List` response/rows must agree.
- TaskDetail and TaskProgressRead re-scan and reject missing fresh identities.
- Learning and test tasks remain distinct source types but both are Routine.
- Expiry, completion, progress, score and raw time remain independent facts.
- Non-zero `time_spent` is treated as bounded milliseconds and converted to
  whole Domain seconds only after fresh Task rebinding; absent or >100-year
  observations fail the standalone DurationRead contract.
- Legacy response decoding is bounded and exact-version only; `jv=99` requires
  a fresh account-bound Capture context and zeroizes crypto JSON, key and
  plaintext buffers.
- `topic_code` appears only in ephemeral zeroizing route/attempt state and is
  absent from serialized Question fixtures and immutable Drafts.
- Request-vector tests freeze `StartAnswer`, `VerifyAnswer`,
  `SubmitAnswerAndSave`, `SkipAnswer` and `SubmitChoseWord` field/sign order.
- Native-boundary tests cover both `ClassTask` and `StudyTask` route families,
  preserve the donor's read/submit `Authorization-v` split for every mutation,
  and reject any operation outside the audited five-operation allowlist.
- `SubmitChoseWord` accepts only its acknowledgement response family; the
  assessment-step parser still rejects missing decoded data.
- `SearchWord` parsing bounds both JSON and the donor's literal
  Unicode-escape layer before extracting a prototype.
- Phrase and example evidence are not merged; completion evidence fetches only
  prefix-matching words which need the example fallback.
- Donor random/fixed-third/last-word fallbacks are stable, low-confidence and
  explicitly identified in answer provenance. Nested sentence fixtures also
  distinguish top-level evidence loading/order from flattened children:
  semantic matches select the exact child wire tag, while a failed match
  retains the donor's third-parent tag.
- Attempt tests freeze one-operation-at-a-time start/verify/advance/skip,
  sequential rotated tokens for matching, reading-card execution, word
  selection acknowledgement, ambiguity no-replay and semantic fail-closed.
- Submission verification replays no mutation: fresh 100%/Completed confirms
  the Task goal while per-Question answer history stays explicitly Unverified;
  a terminal receipt with incomplete readback remains Pending, and stale
  Draft/preview/state combinations fail closed.
- Unknown response fields are dropped.
- Capability advertisement follows implemented registry slots; an unfinished
  shared integration is recorded as a concrete Core Gap, never as a policy
  exclusion or Provider completion boundary.
