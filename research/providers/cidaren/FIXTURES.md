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
  tasks/task-score-success.json
  answers/course-page.json
  answers/study-task-info.json
  answers/study-word-info-envelope.json
  answers/study-word-info-means.json
  questions/start-answer-envelope.json
  questions/start-answer-single.json
  questions/start-answer-fill-blank-73.json
  browser/capture-snapshot-token-only.json
  browser/capture-snapshot-composite.json
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
Legacy decoder tests generate synthetic JSON/base64 and apply the exact
versioned fixed-index or sequential-span/five-chunk donor transform in memory.
They cover `3_1021` under both divergent donor algorithms plus public
`3_2265`/`3_2277`, accepting the dual-evidence identifier only when decoding
has one unique result. The synthetic StartAnswer fixture freezes only the
donor-read topic/stem/option shape plus synthetic
`topic_done_num/topic_total` counters; no published or real word/question
payload is copied into the repository. Counter tests keep the remote
completed/total observation separate from local attempt position and reject
inconsistent or unbounded values.

The synthetic mode-73 fixture retains only the public issue 99 structural
facts: two answers, two positive word lengths, no options and placeholder
text/token values. Parser tests require `FillBlank`, answer-count/length
agreement and token omission from serialized Questions. It is not answer-wire
evidence; AnswerResolve and ordinary answer Build/Verify stay fail-closed,
while the distinct explicit Skip Draft/execution path is available.

The BrowserBridge fixtures are synthetic result transport documents generated
from the Provider typed boundary; tests pair each one with a constructor-built
command carrying the same binding. The token-only fixture is recipe version 1 and
contains a request-header `UserToken` with all Composite fields absent. The
Composite fixture is recipe version 2 and contains a placeholder request-header
token plus a structurally valid, non-secret `CDR_LOGIN_INFO` object using the
audited `a`/`b` prefixes; optional `CDR_USER_SESSION` remains null. Both bind
the same placeholder origin, frame, stable Task identity and sequence on the
command and result. Tests must also cover oversized documents, unknown fields,
wrong origin, frame/Task/nonce/sequence mismatch, token-only/Composite field
mixing, malformed JSON objects and crypto-context parse failure. Fixture values
are never real tokens, cookies, browser storage or answer material, and these
documents are observations only until Core performs its own session validation
and credential commit.

Core exchange fixtures should persist only the stable message type and SHA-256
command/result digests. They must never copy the JSON document, `UserToken`,
`CDR_LOGIN_INFO`, `CDR_USER_SESSION` or a decoded `CidarenCaptureSnapshot` into
Domain/storage rows; the Provider typed boundary remains the sole owner of
those values until the Capture credential commit succeeds.

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
  OAuth context; persisted NativeProviderLogin resolution retains that context
  for later validation, while Capture-origin validation keeps its donor header
  family.
- Public-donor token-only Capture validates and resolves only as one
  ProviderSpecific `ProviderAccessToken` from CaptureTool/BrowserExtension;
  unrelated helper origins and token/Composite relabelling fail closed.
- Typed Capture command recovery owns serialized command bytes in a zeroizing
  SecretValue artifact and rejects digest, BrowserSession, Task, sequence or
  recipe drift before any helper result can be accepted.
  TokenOnly and Composite command fixtures freeze the exact compact JSON bytes
  sent to the helper; those same bytes are the encrypted recovery artifact and
  command-digest input, paired with the existing result fixtures. Recovery
  caps this command artifact independently at 4 KiB rather than inheriting the
  much larger bounded result-document allowance.
  A recovered-exchange fixture drops the in-memory issued wrapper, resolves
  only the encrypted artifact plus persisted exchange, rejects a foreign
  recipe and then completes the exact Composite result through fresh rebinding.
  Helper projection fixtures transfer ownership of the same encrypted command
  bytes and confirm the opaque owner is consumed before dispatch. They
  independently bind the actual origin/frame plus Core session/sequence. They
  freeze TokenOnly's request-header-only source and Composite's exact five-
  source union, while rejecting digest/binding drift, unknown fields/modes,
  invalid Task identities, arbitrary selector/script commands and size drift
  before any DOM handler exists.
  Result-construction fixtures map those source enums to Core's exact read
  authorities, retain all dispatch bindings only in Debug-redacted zeroizing
  owners, and round-trip bounded TokenOnly and Composite artifacts through the
  existing result validator. Empty, reordered, wrong-role, extra, non-UTF-8,
  malformed and oversized captured values fail closed; emitted bytes contain
  no `user_session`, selector, script or arbitrary helper field.
  Encrypted result restoration checks the persisted digest before UTF-8/JSON
  parsing: changed bytes are ProtocolDrift, while correctly digest-bound empty,
  oversized or non-UTF-8 artifacts retain their InvalidResponse classification.
  Shared terminal-capability fixtures call request validation first, derive
  Composite solely from the encrypted command artifact, require the exact
  runtime origin/frame/session and fresh Task capability, and round-trip the
  validated two-field credential replacement with its completed exchange.
  A Composite result paired with a digest-valid TokenOnly command cannot
  elevate the command recipe; foreign runtime bindings and receipt-digest
  drift fail closed.
  Public-surface doctests keep command issue, persisted completion and paired
  credential commit available while proving raw result documents, envelopes,
  snapshots and parsers cannot be imported from the Provider crate.
- Class-task pagination is complete and total-consistent before normalization.
- Course identity is `course:{course_id}` and duplicate rows must agree.
- Task identity is `class-task:{release_id}` even when `task_id == -1`.
- Ordinary study identity is `study-task:{course_id}:{list_id}` even when
  `task_id == -1`.
- Selected Course, query and `StudyTask/List` response/rows must agree.
- TaskDetail and TaskProgressRead re-scan and reject missing fresh identities.
- Learning and test tasks remain distinct source types but both are Routine.
- Expiry, completion, progress, score and raw time remain independent facts.
- Submission verification converts a present fresh 0-100 task score to Core
  thousandth-points, accepts the donor's numeric or decimal-string aliases,
  retains `null` as absent, and rejects conflicting/out-of-range values without
  using score as completion proof. Native request tests freeze both score-route
  query families and require the account-bound `UserToken`; an ordinary-study
  `task_id=-1` falls back to its freshly rebound list fact because the donor
  score route cannot carry the stable `list_id`.
- Non-zero `time_spent` is treated as bounded milliseconds and converted to
  whole Domain seconds only after fresh Task rebinding; absent or >100-year
  observations fail the standalone DurationRead contract.
- Legacy response decoding is bounded and exact-version only; `jv=99` requires
  a fresh account-bound Capture context and zeroizes crypto JSON, key and
  plaintext buffers.
- Provider-local parser tests pin sanitized protocol observations for unknown
  numeric `topic_mode`, unknown class/ordinary-study `task_type`, unknown class
  `over_status` and unknown response encoding version. The task/status/mode
  shapes contain only their parsed integer; the encoding-version shape contains
  only ASCII classification and byte length, never the original `jv`, response
  row or body.
- Assessment-envelope observation tests cover both the shared assessment-step
  parser and the separate word-selection receipt parser. Unknown codes and
  malformed root/code/message/data/jv shapes retain only a fixed family, value
  kinds, optional numeric code and data truthiness; synthetic message, answer,
  topic-code, version and payload values must not cross the observation.
- Task-score observation tests keep response-envelope drift separate from
  decoded score-alias drift. They retain only root/code/data/jv value kinds and
  an optional numeric code, or `score/task_score/grade` kinds plus alias count;
  conflicting score values, extra answer/topic fields and `jv` content remain
  absent from the observation.
- Class-page and ordinary-study envelope tests retain only root/code/data value
  kinds, optional numeric code, bounded record/list counts and known field
  kinds. Synthetic row answers, Course IDs/titles and task-list string values
  do not cross the observation; every malformed envelope still rejects the
  complete inventory.
- `Student/Main` shape tests distinguish an ordinary well-formed rejected token
  (Authentication with no observation) from malformed success/profile and
  selected-Course drift. Observations retain only field kinds, numeric success
  code and profile-field count; student name/code/school/class and Course ID
  values remain inside the zeroizing response owner.
- Answer-evidence shape tests cover malformed `StudyTask/Info`, public
  Course-page, `StudyWordInfo` and `SearchWord` data. They assert that only the
  fixed family, field kinds and bounded object/array counts cross the
  observation boundary; synthetic words, meanings and Course/list identities
  remain absent, while a valid-shape identity change remains `RemoteChanged`
  with no observation.
- Known-mode Question shape tests inject incompatible stem, option-content and
  answer-tag types, retaining only field kinds, counts and numeric mode in the
  observation. Synthetic topic code, stem, option and answer-tag values remain
  absent; missing mode is shape drift, unknown numeric mode keeps its existing
  exact classification and invalid caller Task binding has no observation.
- Native OAuth V2 shape tests distinguish ordinary rejected/expired codes and
  decrypted invalid tokens (Authentication with no observation) from malformed
  successful envelopes and handshake drift. Observations retain only field
  kinds/counts, protocol booleans, fixed-AAD match and encoded lengths; OAuth
  code, server public key, salt, IV, ciphertext, version and token values remain
  absent while the complete response still zeroizes on every return path.
- Response-data framing tests distinguish an unknown `jv`
  (`EndpointVersionDrift`) and missing `jv=99` crypto context (unobserved
  Authentication) from malformed known-version data. The latter retains only
  fixed decoder family, data/payload kinds and counts, ASCII/whitespace facts
  and encoded lengths; synthetic encoded content and the exact `jv` remain
  absent.
- Native HTTP response-head tests keep 429 retry timing and 401 Authentication
  unobserved, while redirect and invalid/missing Content-Type attach only fixed
  route/status or header/media-type structural facts. Raw Content-Type, URL,
  response body and credentials remain absent from every observation.
- Native HTTP body-framing tests cover declared and streamed overflow, empty
  body, invalid UTF-8 and a valid bounded JSON body. Failure observations retain
  only fixed route/state, observed byte length and configured maximum; body
  bytes remain absent and already accumulated bytes are zeroized before return.
- Captured shared-secret and salt bytes become `Zeroizing<Vec<u8>>` at the
  base64 decode boundary, so a later field failure or size rejection also
  clears already decoded key material.
- Native OAuth moves the bounded response String directly into the consuming
  bootstrap without a second byte copy. All decoded handshake/payload fields
  are zeroizing from the base64 boundary, including rejected-length paths.
- Parsed Capture result envelopes cannot be cloned. Consuming one moves its
  token and crypto JSON directly into the zeroizing snapshot while replacing
  the source event with an empty value before its own Drop cleanup.
- Capture JSON-object shape checks recursively clear their temporary parsed
  value, including the optional unused browser session, before returning.
- `Student/Main` token validation and selected-Course extraction wrap the
  complete `user_info` JSON tree in an RAII zeroizing owner, including every
  malformed, rejected and early-return path.
- Task word inventory, CoursePage, word-info and SearchWord response trees use
  the same RAII cleanup rule; parser drift and nested lookup failures cannot
  bypass answer-evidence zeroization through an early `?` return.
- Encrypted pre/post-Question wire decoders move validated one-time fields
  into runtime artifacts instead of cloning them. Parser restoration wraps
  topic codes and binding strings in zeroizing owners before its first
  fallible validation, so malformed recovery clears inputs as well.
- `topic_code` is absent from serialized Question fixtures and immutable
  Drafts. The `cidaren.question-attempt.v2` Provider artifact is bounded,
  zeroizing and stable-digest covered for Core's encrypted continuation store;
  decode tests reject foreign local/remote Tasks, Questions, positions,
  fingerprints, unknown fields, digest drift, oversized values and invalid
  `verified_steps` checkpoints.
- Optional `topic_done_num/topic_total` are bounded integers, completed never
  exceeds total, missing legacy payloads remain valid, and the counters neither
  alter Question fingerprints nor drive the local mutation position.
- Question parsing keeps partial stem/options/metadata and duplicate-ID sets
  behind zeroizing guards until the complete normalized Question validates.
  Reading-card identity JSON and hash material are cleared immediately.
- Request-vector tests freeze `StartAnswer`, `VerifyAnswer`,
  `SubmitAnswerAndSave`, `SkipAnswer` and `SubmitChoseWord` field/sign order.
- Mutation serialization consumes and recursively clears its temporary JSON
  tree, including copied answer, topic-code and word-map values, before keeping
  only the zeroizing serialized request body.
- Prepared-request tests prove equal timestamp/path/query/body inputs retain
  one stable durable digest, while timestamp or answer changes produce another
  digest; Provider transport no longer chooses mutation timestamps after the
  operation has entered `Issued`.
- Typed transport outcomes reject zero response digests and preserve the raw-
  response digest plus observation time; artifact rotation tests prove a fresh
  topic code changes the digest without weakening Task/Question bindings or
  accepting malformed replacement state.
- Pre-Question recovery tests cover namespaced selection/start/reading-card
  phases, absence of word-map values from encrypted artifact plaintext, fresh
  plan requirements, local/remote Task rebinding, correlation-independent
  identity and reading-card continuation into the next real Question.
- Matching recovery tests retain one immutable Question ID across each
  sequential Verify rotation while requiring a distinct artifact digest,
  monotonic `verified_steps` and the exact `cidaren.ready-to-verify` /
  `cidaren.ready-to-advance` phase. Recovery regenerates relation identity from
  the immutable selection and rejects an impossible accepted prefix.
- Native-boundary tests cover both `ClassTask` and `StudyTask` route families,
  preserve the donor's read/submit `Authorization-v` split for every mutation,
  and reject any operation outside the audited five-operation allowlist.
- The non-redirecting Native HTTP reader clears every body chunk already
  accumulated before returning a mid-stream transport error or size failure;
  no Provider layer performs an automatic assessment retry.
- `SubmitChoseWord` accepts only its acknowledgement response family; the
  assessment-step parser still rejects missing decoded data.
- Mode 73 parses as FillBlank only when its bounded answer count equals its
  word-length count; no answer wire shape is inferred from the public log.
- `SearchWord` parsing bounds both JSON and the donor's literal
  Unicode-escape layer before extracting a prototype.
- Phrase and example evidence are not merged; completion evidence fetches only
  prefix-matching words which need the example fallback.
- Word-evidence parsing owns word/meaning/example clones behind cleanup guards;
  a later duplicate, missing translation or collection-bound failure clears
  every already accepted field and the rejected replacement value.
- Empty meaning parts and option meanings reduced to empty text after removing
  Chinese parentheticals are protocol drift, never valid answer evidence.
- Donor random/fixed-third/last-word fallbacks are stable, low-confidence and
  explicitly identified in answer provenance. Nested sentence fixtures also
  distinguish top-level evidence loading/order from flattened children:
  semantic matches select the exact child wire tag, while a failed match
  retains the donor's third-parent tag.
- Attempt tests freeze one-operation-at-a-time start/verify/advance/skip,
  sequential rotated tokens for matching, reading-card execution, word
  selection acknowledgement, ambiguity no-replay and semantic fail-closed.
  The registered SubmissionExecute adapter also restores a two-relation Draft
  across both rotated-token revisions, reaches ReadyToAdvance only after the
  second Verify and then accepts one terminal advance without a continuation.
  A full post-Question reselection fixture persists the next position across
  `SkipAnswer -> SubmitChoseWord -> StartAnswer` and materializes Question 2,
  preventing recovery from silently returning to Question 1.
  A synthetic missing internal phase stays Debug-safe, reports `Invalid` and
  returns a typed Internal error instead of panicking or inventing a mutation.
  Issued start/verify/skip commands have no hidden preflight wait; verified
  advance freezes 1-2 seconds, reading-card advance freezes 1-3 seconds, and
  the independently configurable post-step delay remains 2 seconds by default.
- Runtime-setting tests freeze schema revision 2, donor-default 2-second
  inter-question delay and its UI-evidenced lower bound, while retaining
  Provider/account/Task overrides and stable per-step selection.
- Submission verification replays no mutation: fresh 100%/Completed confirms
  the Task goal while per-Question answer history stays explicitly Unverified;
  a terminal receipt with incomplete readback remains Pending, and stale
  Draft/preview/state combinations fail closed.
- Completion-diagnosis tests freeze only evidenced facts: explicit `Expired`
  with incomplete progress is `WindowClosed`; `NotOpen/Pending/InProgress`,
  completed and unknown states return no Provider diagnosis because Task-level
  progress does not identify pending children. Score never supplies a passing-
  threshold inference, and Cidaren has no TaskExecution slot to diagnose.
- Those diagnosis regressions also freeze the post-`1824fb4` continuation
  boundary: Cidaren emits no retry-eligible completion reason, so Provider
  fixtures cannot be used as implicit StartAnswer/retake authority.
- Coverage regression tests freeze Cidaren's no-partial-policy boundary:
  complete one-Question Answer and explicit Skip Drafts use 1000/1000 with no
  unanswered IDs, while a generic Domain-valid 50% Draft is rejected before
  remote verification. SubmissionExecute applies the same shared predicate
  before any Task read or mutation.
- No fixture fabricates a history/result/retake endpoint. The full donor sweep
  found no per-Question correctness readback, passing threshold or retake
  eligibility shape; `chance_num` remains current-Question data. Shared corpus
  bootstrap and Score Improvement must preserve that absence, while Strict
  Completion can consume the existing fresh 100%/Completed fixture.
- Unknown response fields are dropped.
- Capability advertisement follows implemented registry slots; an unfinished
  shared integration is recorded as a concrete Core Gap, never as a policy
  exclusion or Provider completion boundary.

## Full-sweep evidence coverage

The one-time full upstream sweep found no donor-owned automated test or fixture
directory. Existing sanitized fixtures cover every reusable platform shape
found in source, inline examples, public issues and the previously verified
release archive:

- auth fixtures cover opaque token acceptance/rejection; OAuth V2 remains a
  deterministic crypto/request vector rather than copied live credentials;
- class/study fixtures cover complete pagination, selected Course, stable
  release/list identity, `task_id=-1`, progress, duration and score;
- answer fixtures cover CoursePage, StudyTask Info, StudyWordInfo and prototype
  evidence families used by every source answer mode;
- Question fixtures cover ordinary, reading, matching/nested tags and the
  issue-99 mode-73 structural boundary;
- Browser fixtures cover both exact Capture command/result recipes and their
  durable credential terminal.

Historical tag inspection added no distinct executable response shape. The old
`get_all_task` function has no request and no caller; issue requests for direct
cumulative-duration mutation, batch-all UI and additional self-study selection
were never implemented upstream. They therefore do not justify synthetic wire
fixtures. The complete evidence-to-classification ledger is
`FULL_UPSTREAM_SWEEP.md`.
