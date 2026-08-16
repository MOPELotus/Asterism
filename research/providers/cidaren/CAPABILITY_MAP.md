# Cidaren capability map

This map combines the reopened `ularch@bce9559`, owner-supplied
`MOPELotus@a74b4a2` and OAuth V2 assisted-bootstrap handoff. Their additive
differences are frozen in [`DONOR_DIFFERENCES.md`](DONOR_DIFFERENCES.md).
The mandatory from-scratch comparison against every recorded README, config,
tag/release, issue, example, fixture and implementation call site is recorded
in [`FULL_UPSTREAM_SWEEP.md`](FULL_UPSTREAM_SWEEP.md).

| Asterism capability | Primary evidence | Use | Current decision |
|---|---|---|---|
| Authentication | Current donors + first-party H5/live-safe capture + assisted-bootstrap handoff | PortSource + Reference | Manual opaque `UserToken`, public-donor token-only Capture, captured Composite sessions and the assisted native V2 path are implemented at the Provider validation boundary. `AuthenticationCapability` returns the structured External OAuth challenge for both assisted/browser-OAuth begins and consumes Core-claimed callbacks into a Composite replacement; imported-token begins carry no OAuth artifact. Provider generates independent CSPRNG `state`/random `authorize != "2"`, maps only their hashes into the shared binding, strictly parses query or SPA-fragment callbacks, sends verified `Authorization-v: 00` with exact User-Agent/derived `Abc`, performs fresh P-256/SPKI + signed ECDH/HKDF/AES-GCM once, validates through same-context fresh `Student/Main`, and yields atomic Composite material. Core now durably binds owner/account/AuthSession/TTL, atomically claims/consumes the two digests, records terminal or ambiguous outcome and commits the NativeProviderLogin replacement; Password is not invented without evidence |
| Stored session validation | Current donors + Asterism secrets boundary | FromScratch | Resolver accepts one exact manual or CaptureTool/BrowserExtension token-only ProviderSpecific session, or an exact atomic Composite token + crypto pair from NativeProviderLogin/CaptureTool/BrowserExtension, all account/reference/purpose/session/acquisition/timestamp bound. NativeProviderLogin preserves its non-secret validation context across resolution and reuses the current OAuth header family; Capture/manual sessions retain donor headers |
| Session recovery / refresh | Current donor | Reference | No audited refresh endpoint exists; expired sessions require fresh manual import, random-marker OAuth bootstrap or Capture-assisted reacquisition rather than silent refresh claims |
| CourseInventory | Current/private task routes + public issue fixtures | PortSource | Native signed class pagination and selected-Course `StudyTask/List` are merged by stable `course_id`; conflicting titles fail the complete inventory |
| TaskInventory | Current/private donor + public issue 83 | PortSource | Native `ClassTask/PageTask` pagination is all-or-nothing; class learning/test identities bind to `release_id`, while ordinary study units bind to `course_id + list_id` because `task_id` may remain `-1` |
| TaskDetail | Current donor + Asterism Core | FromScratch | Offline/native-boundary covered: freshly re-list the matching class or study endpoint and require one exact stable identity before returning details; never trust a stale `task_id` |
| TaskProgressRead | Current/private task rows | PortSource | Offline/native-boundary covered for class and ordinary study Tasks: fresh rows expose bounded percent/state plus independently converted duration when present; score and completion remain separate |
| DurationRead | Current donor + public issue 6 | PortSource | Registered and fresh-read for class/study Tasks with `time_spent`: non-zero public rows, millisecond epoch/duration fields in the same envelope and donor mutation values establish millisecond wire semantics; values are bounded to 100 years and truncated only at the Domain seconds boundary |
| QuestionInventory / QuestionParse | Current donors + public issues 43/70/77/85/99/106 | PortSource | Fixed-index `jv=2_*`, both evidenced `jv=3_1021` transforms, public `jv=3_2265/3_2277` and current authenticated `jv=99` decoding plus strict single/matching/text/nested-tag parsing are offline-covered; conflicting `3_1021` candidates must yield one unique/equal JSON value. Public issue 99's donor-unsupported mode 73 is now parsed as a bounded two-blank Question with answer-count/word-length agreement, allowing the evidenced Skip path without guessing its unknown multi-answer wire encoding. Its optional paired `task_id/task_type` echo is checked against the fresh Task row before materialization; positive known IDs and row type must match. A fresh `-1` may receive a positive dynamic ID without promoting it to stable identity; public multi-step logs establish that the allocation remains constant while topic tokens rotate, so both pre-Question and real-Question encrypted continuations retain it and reject a later response/fresh row that switches allocation. The rotating `topic_code` remains absent from Domain Questions and Drafts; a stable Provider-private artifact binds it to local/remote Task, Question ID, position and content fingerprint for encrypted Core storage. Public-donor `topic_done_num/topic_total` remain an optional bounded remote progress observation distinct from local attempt position and now round-trip through both reading-card and real-Question encrypted recovery. The registered public QuestionInventory adapter prepares the initial encrypted state, freshly restores each phase, exposes only the frozen operation type/digest/delay, and maps accepted results to Continue, real-Question Materialize or definite Completed. Ambiguous recovery returns no result because no donor readback exists. Core/Engine/API now durably consume direct materialization through `GET /api/v1/tasks/{task_id}/questions`; the ordinary read-only QuestionParse slot is intentionally not fabricated |
| AnswerResolve | Current donor | PortSource | Native task-bound inventory, `StudyWordInfo` and bounded `Course/SearchWord` prototype lookup are implemented and registered as a public capability. The session-aware Core hook requires the exact current `cidaren.question-attempt.v2` revision/type/phase, then decrypts and rebinds its digest, Task, Question identity, position and content fingerprint before any evidence read. Donor strategies resolve bidirectional meaning, ordered matching, separately-scoped phrase/example evidence and completion/example behavior; audited random/fixed-third/last-word failure fallbacks are retained with stable Draft-safe selection and explicit low confidence, including well-formed empty evidence families. Sentence modes load evidence once per top-level parent, preserve donor ordering, resolve exact nested child wire tags and retain the donor's third-parent fallback |
| SubmissionBuild | Current donor | Reference | Registry-advertised one-current-Question immutable Draft preview is implemented for selection/text/matching and explicit Skip. Answer previews contain only Verify/advance field names; Skip has a distinct format with only `skip.topic_code` and `skip.time_spent`. Neither form persists topic codes, signatures, endpoints or answer values. Cidaren declares no partial-submit setting: its Provider-private coverage predicate requires 1000/1000, no unanswered IDs and total count equal to the current Draft item count |
| SubmissionExecute | Current donor | PortSource | Registered native execution plus a one-shot Provider state machine cover `SubmitChoseWord`, `StartAnswer`, sequential `VerifyAnswer`, `SubmitAnswerAndSave` and `SkipAnswer`. The session-aware adapter revalidates Domain coverage plus Cidaren's exact complete-coverage policy and immutable Draft preview before any fresh read or mutation, freshly rebinds the Task, restores either question or pre-question artifacts and freezes exactly one operation. Same-Question Verify rotates Continue; definite next Questions atomically consume the old session and create a new immutable Snapshot/session while returning the Task to Ready; reading-card/selection stages rotate the pre-question artifact; terminal Completed closes the session without inventing a continuation and proceeds to fresh verification. Issued/ambiguous/failed-closed states prevent replay and ambiguity recovery remains `None` |
| SubmissionVerify | Fresh class task/detail read + current public donor score read | FromScratch | The read-only verifier is implemented and registered independently of mutation execution. It is complete-coverage/Draft/preview/task bound: a receipt or localized completion message is insufficient, fresh exact release/list completion is required, and the independently observed bounded 0-100 score is converted to Core thousandth-points when present. A structurally valid generic partial Draft fails before remote verification. Its verified completion diagnosis maps only explicit class-task `Expired` with incomplete progress to `WindowClosed`; unfinished, unknown or inconsistent facts remain unmapped. Per-Question status remains honestly `Unverified` because no answer-history endpoint is evidenced. SubmissionExecute is integrated with durable Core Attempt/session transitions and verification remains a separate fresh phase |
| Capture bootstrap | Current donors + PC WeChat XWeb audit | PortSource + Reference | Token-only and Composite ingestion plus separate bounded token-only request-header and Composite browser-storage recipes are implemented and advertised as ordered alternatives. Core freezes one exact version per bootstrap and never mixes their outputs. Cidaren validates a typed Provider-private `CaptureSnapshot` command/result for those recipes, including exact origin/frame/Task/sequence binding and `jv=99` context parsing, then converts only an exact token-only or atomic token+crypto replacement for the shared credential commit; the donor-observed but unused `CDR_USER_SESSION` is not persisted. The shared runner executes those exact reads and terminal result construction, but its isolated Edge/Chrome profile cannot inherit an authenticated PC WeChat XWeb context or reproduce the donor's system-proxy lifecycle. Acquiring that external browser state and validating it live remain shared environment gates. OAuth bootstrap no longer depends on XWeb/MITM: users can return the preserved random-marker callback URL from an audited WeChat device flow |
| BrowserBridge | Current donor | PortSource | Provider policy revision 3 is implemented and advertised for freshly rebound class/study Tasks: visible, account/task hash-isolated, started at the audited `https://app.vocabgo.com/student/` page and restricted to its exact `https://app.vocabgo.com` origin plus the exact read-source union (`usertoken` request header, local/session `CDR_USER_TOKEN`, local/session `CDR_LOGIN_INFO`). The Provider has a typed CaptureSnapshot command/result boundary (TokenOnly v1 and Composite v2), fresh-Task command construction/result parsing and an immutable adapter to Core's durable one-shot ledger. It derives the command nonce from the exact Core session ID, freezes type/digest/sequence at issue, repeats fresh rebinding before acceptance and returns only zeroizing Provider-private Capture material plus terminal hash metadata. Helper-side projection authenticates Core's exact command artifact and independently binds actual origin/frame/session/sequence before returning only the TokenOnly or Composite action and that mode's fixed source subset; arbitrary selector/script shapes fail strict decoding. The shared Chromium runner executes the projection and fixed reads, while Core persists exact encrypted command/result artifacts and atomically commits the credential terminal. Only authenticated-context acquisition/live validation remains; Cidaren assessment mutations remain native HTTP because no donor browser action for those routes was evidenced |

## Current implementation checkpoint

The current checkpoint (not a completion boundary):

1. establish a Development-only Provider crate with exact canonical ID
   `cidaren`;
2. validate bounded, redacted and zeroized manual/token-only Capture and
   captured Composite sessions;
3. classify `Student/Main` success, ordinary token expiry and malformed
   responses without retaining account PII;
4. parse all-or-nothing class-task pages into unique Courses and stable Tasks;
5. normalize learning tasks as routine Practice and test tasks as routine Exam,
   without treating either source type as a formal assessment;
6. read the account-selected Course plus ordinary `task_type=3` study units
   through bounded `StudyTask/List`, merging Course evidence without hiding
   title conflicts;
7. retain progress, score, access flags and raw timing independently;
8. use `class-task:{release_id}` for class Tasks and
   `study-task:{course_id}:{list_id}` for ordinary study Tasks, preserving
   `task_id` only as a fresh observation;
9. compose Authentication, BrowserBridge, CourseInventory, TaskInventory,
   TaskDetail, TaskProgressRead, SubmissionBuild and SubmissionExecute slots into one
   registry-consistent Development entry;
10. resolves exact Core account/reference-bound manual or Composite sessions
    and uses one shared non-redirecting HTTPS client for account validation and
    both task families;
11. freezes donor headers, request/body signing-version split, selected-Course
    query binding, JSON/status/body bounds and sensitive `UserToken` handling
    with offline tests;
12. re-lists the matching native inventory for every detail/progress/duration
    read, rejects malformed or disappeared stable identities and converts only
    bounded `time_spent` milliseconds into Domain seconds;
13. decodes exact legacy base64/confusion variants and current `jv=99` through
    HKDF-SHA256/AES-256-GCM with bounded zeroized crypto material;
14. advertises Capture-assisted authentication, BrowserBridge, AnswerResolve,
    SubmissionBuild, SubmissionExecute and SubmissionVerify, and parses current Questions without persisting
    `topic_code`;
15. freezes fresh binding and exact request/signature vectors for
    `StartAnswer`, `VerifyAnswer`, `SubmitAnswerAndSave`, `SkipAnswer` and
    `SubmitChoseWord`, including the donor's distinct class Start selector
    (`2` for both learning/test) versus decoded row identity (`1` learning,
    `2` test);
16. normalizes bounded word-info evidence and donor answer strategies, then
    loads only the task-bound word evidence needed by the current Question,
    including bounded `SearchWord` prototype and completion-example fallback;
17. models the assessment lifecycle as one-shot issued commands: word
    selection must be acknowledged before start, matching Verify calls consume
    rotated topic codes sequentially, reading/skip/advance remain distinct,
    and ambiguous or semantically invalid results cannot be replayed;
18. exposes runtime schema revision 2 with eight provider/account/task settings
    for scheduler concurrency/scan policy and donor answer
    delay/reported-duration behavior, including the evidenced 2-second delay
    default and lower bound,
    with stable per-step range selection;
19. retains donor failure fallbacks without hiding their quality: random choice
    is stable for an immutable Question, fixed-third and last-word paths remain
    explicit, and all carry low confidence plus sanitized provenance; nested
    sentence options retain their top-level identity so semantic matches use
    the exact child wire tag and the fixed-third failure path uses the exact
    third parent tag rather than the third flattened child;
20. implements the current assisted OAuth bootstrap and first-party V2 exchange
    as a one-shot Provider-native operation: independent CSPRNG state/marker,
    hash-only durable binding, exact callback allowlist and query/fragment
    parser, verified `Authorization-v: 00` plus User-Agent/Abc, fresh P-256/SPKI,
    sorted signed request,
    ECDH/HKDF/AES-GCM authenticated response, exact Composite material and
    mandatory fresh account readback, with no ambiguous retry.
21. registers the independent native AnswerResolve and fresh SubmissionVerify
    capabilities while retaining the reopened donor's bounded current-attempt
    completed/total
    counters on Question and reading-card steps without folding them into the
    local position, Question fingerprint or mutation transition.
22. maps each typed Capture command/result into Core's durable exchange
    metadata without exposing mutable type/digest/sequence assembly: the
    session ID is the command nonce, result acceptance repeats fresh Task
    rebinding, and credential values never enter Domain storage.
23. converts the validated snapshot into Core's existing zeroizing
    `CredentialReplacement` boundary: token-only and Composite shapes remain
    distinct and optional unused browser session data is discarded.
24. preserves the actual one-shot response observation time through outcome
    acceptance so delayed durable recovery cannot fabricate a later terminal
    receipt timestamp.
25. mirrors the donor's compound response success condition while retaining
    public issue 72's exact `20001 + 需要选词！ + data=null` remote-state
    exception; unrelated empty `20001` payloads fail closed and `20004` is a
    terminal receipt that cannot trigger another Start mutation.
26. classifies other localized terminal/word-selection text only after the
    numeric/data success gate, so unrelated remote failure text cannot
    manufacture a receipt.
27. parses public issue 99's `topic_mode=73`, no-option, two-length payload as
    a bounded `FillBlank` Question and preserves Skip execution, while leaving
    AnswerResolve/Verify unsupported until evidence defines the two-answer
    wire encoding.
28. encodes the current Question's one-time `topic_code` as a bounded,
    versioned `cidaren.question-attempt.v2`, zeroizing Provider artifact whose
    digest is stable and whose
    decode path rebinds the local Task, remote Task, remote Question, position
    and complete Question content fingerprint. It also retains only the
    bounded number of already accepted Verify relations, allowing the exact
    remaining sequence to be rebuilt from the immutable Draft without copying
    answer content into the artifact, plus the paired bounded remote progress
    observation so recovery does not erase donor state. The plaintext is
    intended only for Core's encrypted QuestionSession continuation store and
    never enters a normalized Question, immutable Draft or diagnostic output.
29. freezes every assessment mutation's timestamp, path, ordered query or
    serialized signed body before the Provider-private flow enters `Issued`;
    the command exposes a stable `cidaren.*.v1` operation type and framed
    SHA-256 request digest for Core's operation ledger, while Native transport
    can only send the already prepared request.
30. hashes the bounded raw response before its zeroizing buffer is cleared and
    carries that non-zero digest with the actual receive timestamp through the
    typed transport outcome. Accepted `VerifyAnswer` state can replace only the
    topic code while retaining every immutable artifact binding; all Provider
    continuation phases use the required `cidaren.*` namespace.
31. exposes the audited attempt flow and materialization bundles as a public
    Provider-private integration surface: a real Question bundle carries the
    normalized Question, zeroizing artifact, phase, raw response digest and
    observation time, while matching Verify rotations retain the same
    immutable Question binding and monotonically advancing verified-step
    checkpoint across every topic-code revision.
32. encodes pre-Question selection/start/reading-card state as the bounded
    `cidaren.pre-question-attempt.v2` artifact. Recovery rebinds local/remote
    Task identity, requires a fresh word-selection plan instead of persisting
    its large map, restores reading-card progress and allows correlation to
    change because it remains audit metadata rather than execution identity.
33. registers a public `QuestionInventoryCapability` pre-Question adapter. It
    resolves word-selection evidence only from fresh Task-bound inventory,
    decodes and rebinds every encrypted phase, freezes exactly one command,
    and maps the accepted raw response to `Continue`, real-Question
    `Materialize`, or typed `Completed`. A repeated word-selection requirement
    rotates only to the reselection phase, so the next operation rebuilds the
    map from fresh evidence; ambiguous recovery returns `None` because neither
    donor exposes a safe current-attempt readback.
34. clears Provider-owned protocol intermediates on every exit path: Capture
    command serialization zeroizes the Core session nonce after hashing, and
    answer-evidence parsing/loading zeroizes partial word inventories,
    prototype aliases, completion prefixes and normalized deduplication keys
    across duplicate, identity-drift and asynchronous transport failures. The
    high-level BrowserBridge result boundary now also consumes one bounded,
    Debug-redacted owner for the raw credential-bearing JSON, so parse,
    digest, fresh-rebinding and completion errors cannot leave the Provider's
    transport copy uncleared.
35. consumes Core's resolved encrypted QuestionSession continuation in the
    public AnswerResolve path. Only one current Question, artifact revision 1
    and the initial current-Question phase are accepted; Provider decoding
    rechecks the continuation digest and complete Task/Question fingerprint
    before loading any remote answer evidence.
36. registers the post-materialization SubmissionExecute adapter. A persistent
    `NormalizedAnswer::Skip` intent reaches only `SkipAnswer`; ordinary answers
    serially Verify then advance. Accepted results are classified once as a
    same-session continuation, pre-question continuation, next-Question
    materialization or terminal receipt, and ambiguity never authorizes replay.
    Pre-Question reselection/start artifacts retain the bounded next position,
    so a mid-attempt `WordSelectionRequired` response cannot regress recovery
    to the first Question; legacy position-less v1 artifacts fail closed.
37. exercises multi-relation matching through the registered durable adapter,
    restoring the immutable Draft after each accepted Verify, requiring a new
    topic-code digest before the next relation and advancing only after the
    exact verified relation count reaches the Draft-bound answer length.
38. encodes each typed Capture command into a zeroizing encrypted-at-rest
    artifact whose digest is the exchange command digest. Recovery decodes it
    only after exact digest, BrowserSession, Task, sequence and recipe rebinding;
    helper-echoed result fields never reconstruct command authority. Issued
    and completed wrappers expose consuming all-or-nothing ownership handoffs,
    keeping command/artifact/exchange and snapshot/exchange paired.
39. completes Capture results from a Core-resolved encrypted command artifact
    after process recovery. The Provider first validates the persisted Issued
    exchange, decodes and rebinds command authority, then runs the same fresh
    Task/result parser; a helper-selected recipe is rejected before acceptance.
40. consumes a completed Capture only as snapshot + terminal exchange or as
    credential replacement + terminal exchange. No consuming API can yield
    the captured secrets while silently discarding the result-ledger half.
41. restores one Core-encrypted raw Capture result only with its persisted
    metadata tuple. Provider validation rechecks result type, BrowserSession,
    sequence, exact raw-byte digest and non-regressing receive time; completion
    uses that stored receive time, so callers cannot supply those fields as
    independent acceptance authority. Command-only recovery remains an
    internal implementation step and is not an alternate public result-
    acceptance entry. Raw result envelopes, parsers, snapshots and snapshot-
    only handoffs are also crate-private; the external runtime can consume only
    the persisted completion's credential replacement + terminal exchange pair.
42. strictly decodes one Core-dispatched command artifact before any helper DOM
    handling. Digest, schema, command revision/mode, BrowserSession nonce,
    actual origin/frame, sequence and Cidaren Task grammar are rebound without
    helper echoes; output is only TokenOnly/Composite CaptureSnapshot plus each
    mode's fixed audited source alternatives, never a selector or script.
43. maps every closed helper source directly onto Core's exact read-source
    authority, then consumes the projection and ordered captured secrets into
    one bounded, zeroizing `cidaren.capture.snapshot.result` artifact. TokenOnly
    accepts only request-header `UserToken`; Composite requires one allowed
    `UserToken` followed by one allowed `CDR_LOGIN_INFO`. Existing event
    validation rechecks every retained Debug-redacted binding, and no
    `CDR_USER_SESSION`, selector, script, duplicate or arbitrary field can be
    emitted.
44. implements Core's terminal `BrowserBridgeCapability` credential-result
    hook. Shared request validation runs first; Cidaren then derives the sole
    TokenOnly/Composite authority from the digest-bound command, rebinds its
    session/Task/origin/frame to the runtime observation, repeats fresh Task
    policy validation through persisted completion and returns only a
    `BrowserBridgeCredentialResult::try_new`-validated replacement + completed
    exchange pair. Result bytes and caller metadata cannot select the recipe.
45. decorates malformed or unrecognized assessment-step and word-selection
    envelopes with one bounded `UnknownResultShape` observation. It retains
    only the fixed family, JSON value kinds, an optional numeric result code
    and data truthiness; messages, payload values, answers, topic codes and
    response-version strings remain outside the observation. Existing
    `ProtocolDrift`/`InvalidResponse` classification and ambiguous no-replay
    behavior are unchanged.
46. observes terminal task-score envelope and decoded-alias drift separately
    from assessment mutation receipts. Envelope observations retain only JSON
    kinds and an optional numeric code; decoded observations retain only the
    `score`/`task_score`/`grade` kinds and alias count. Score values, unrelated
    payload fields and answer/session material remain excluded, while fresh
    Task completion/progress continues to be the independent verification fact.
47. attaches bounded shape-only observations to class-page and selected-Course
    study-list envelope drift. Only root/code/data field kinds, optional numeric
    code, structural counts and the known Course/list field kinds are retained;
    row content, Course/Task identities, titles, score/progress values and
    authentication material remain excluded. The complete inventory still
    fails atomically before any stale Task can reach terminal readback.
48. observes malformed successful `Student/Main` account envelopes and selected
    Course field drift without observing ordinary rejected/expired tokens.
    Only root/code/data/user-info/Course field kinds, numeric success code and
    profile field count are retained; profile keys/values, student identity,
    Course ID and credential material remain excluded. Imported, Capture and
    native OAuth sessions continue through the same fresh account validation.
49. observes malformed answer-evidence envelopes and decoded inventory/word
    shapes across `StudyTask/Info`, public Course-page, `StudyWordInfo` and
    `SearchWord`. Only the fixed route family, JSON kinds, optional numeric
    code, object/array counts and known word/list/evidence field-kind counts are
    retained. Word, meaning, example, Course/list identity and raw response
    values remain excluded; binding changes stay unobserved `RemoteChanged`.
50. observes known-mode Question and reading-card payload-shape drift while
    preserving the existing exact unknown-mode observation. Only root/known
    field kinds, object/array counts, field-kind counts and numeric topic mode
    are retained; topic code, stem, option, answer tag and matching content are
    excluded. Invalid caller Task/position binding remains unobserved local
    protocol failure rather than remote drift.
51. observes malformed successful native OAuth V2 envelopes and authenticated
    handshake/payload drift. Only root/known field kinds and counts, numeric
    success code, protocol booleans, fixed-AAD match and encoded string lengths
    are retained; OAuth code, public key, salt, IV, ciphertext, token and
    version values remain excluded. Ordinary rejected/expired codes and
    decrypted invalid-token Authentication remain unobserved.
52. observes malformed data framing for every exact known response decoder
    family, including plain/base64, fixed-confusion, chunked-confusion and
    authenticated AES-GCM. Only the fixed family, data/payload kinds and counts,
    ASCII/whitespace facts and encoded field lengths are retained; encoded,
    AAD and ciphertext values remain excluded. Unknown `jv` keeps its smaller
    `EndpointVersionDrift` shape and missing `jv=99` context stays unobserved
    Authentication.
53. observes protocol-level Native HTTP response-head drift across every fixed
    route. Redirect/404 and other unexpected non-success statuses retain only
    route plus status; missing/invalid/non-JSON Content-Type retains only header
    presence/UTF-8, media-type length/ASCII, parameter count and JSON-suffix
    fact. Header values, URLs, bodies and credentials remain excluded, while
    401/403, 429 and 5xx stay unobserved operational errors.
54. observes Native HTTP JSON-body framing failures after a successful response
    head. Declared-length overflow, streamed overflow, empty body and invalid
    UTF-8 retain only fixed route/state, observed byte length and route maximum;
    body bytes remain zeroized and excluded. Chunk-read transport failures keep
    their existing unobserved Network/InvalidResponse classification.
55. observes class and ordinary-study Task row field-shape drift after a valid
    complete envelope. Only row kind, object field count and fixed known field
    kinds are retained; Task/Course/list identity, title, score, progress and
    time values remain excluded. Existing unknown task/status observations are
    preserved without replacement and the complete inventory still fails
    atomically.
56. validates one complete JSON value at the Native HTTP body boundary before
    any route parser receives it, using a non-retaining deserializer. Invalid
    syntax or trailing data zeroizes the owned body and retains only fixed
    route, `invalid_json`, observed length and configured maximum in the body
    observation; response content and parsed trees remain excluded.

The registered pre-Question adapter now runs through Main-owned durable
Engine/API orchestration. Post-materialization QuestionSession execution now
integrates explicit Skip, terminal-without-artifact and atomic next-Question
transitions across Provider, Storage and Engine. The shared Chromium runner now
executes the exact Cidaren command/read/result projection; remaining work is
authenticated-browser acquisition and live validation rather than another
Provider command or credential path.
Fresh post-mutation verification and the durable one-shot External OAuth path
are already implemented. A checkpoint or Core Gap is not a Provider stopping
condition.

Cidaren is complete only when all audited donor capabilities have current
executable implementation/verification, or every remainder has a concrete
hard blocker that cannot be resolved through further audit, Capture,
BrowserBridge, protocol work or a Main-owned Core change.

## Concrete remaining blockers

No current donor call site remains both unimplemented and solvable through a
Cidaren-only code change. The remaining executable gates are:

1. **Authenticated BrowserBridge context (`CIDAREN-BLOCKER-BROWSER-CONTEXT`).**
   The shared runner launches a new temporary Chromium profile and now fully
   executes Cidaren's strict command, fixed read-source set and typed terminal
   result. The public donor instead captures a system-wide proxied request,
   while the owner donor reads an already authenticated WeChat XWeb document.
   A fresh profile has neither state. Resolution requires an authorized user
   login in that profile or a shared attach/proxy/XWeb acquisition boundary,
   followed by live proof that `UserToken` and `CDR_LOGIN_INFO` are observed
   from the same bound document. No Provider literal, parser or result path is
   missing.
2. **Real-account protocol validation (`CIDAREN-BLOCKER-LIVE-ACCOUNT`).**
   Synthetic fixtures cover every audited route and transform, but cannot
   establish current account binding, inventory vocabulary, non-zero duration,
   post-run score or `jv=99` behavior against production. Resolution requires
   an explicitly supplied test account and sanitized captures; metadata remains
   Development until the live gates in `DRIFT.md` pass.
3. **Authorized mutation validation (`CIDAREN-BLOCKER-LIVE-MUTATION`).**
   `SubmitChoseWord`, `StartAnswer`, `VerifyAnswer`,
   `SubmitAnswerAndSave` and `SkipAnswer` are implemented with durable no-replay
   semantics, but production validation changes remote learning state.
   Resolution requires explicit live-test authorization and an eligible Task;
   offline retries or fabricated receipts cannot remove this blocker.
4. **Mode 73 answer encoding (`CIDAREN-BLOCKER-EVIDENCE-73`).** Public issue 99
   and the donor source expose the two-blank payload while explicitly leaving
   direct answer encoding unimplemented. Asterism already preserves the
   evidenced explicit Skip path. Answer execution can expand only after a new
   upstream commit or authorized trace establishes the ordered multi-answer
   wire shape; guessing it is not a remaining donor capability port.
5. **Pre-Question child prerequisite (`CIDAREN-CORE-GAP-QUESTION-BLOCKED`).**
   Public issues 48/49 repeatedly show `SubmitChoseWord` definitely rejected
   with `code=0`, null data and an exact incomplete-child message. The shared
   Question read outcome cannot store the response digest/time plus
   `RequiredChildrenPending` without misclassifying the Task or authorizing a
   replay. Main must add a definite blocked remote-step outcome first.

No current Core contract blocker remains for Cidaren's typed Capture credential
terminal; the Question blocked-outcome gap above is separate. No refresh or per-Question answer-history route is present in any
audited donor, so those absences are evidence boundaries rather than deferred
Provider implementations.

The shared owner-global Answer Evidence Corpus and the two default-on policy
state machines require one explicit Main-owned representation boundary:

- Cidaren has no historical-attempt or per-Question correctness readback, so
  first-bind bootstrap yields no verified answer records and ordinary
  SubmissionVerify can contribute only task-level completion/score;
- Strict Completion is supported by fresh exact `Completed + 100%` and is
  independent of the optional score; no separate passing threshold exists;
- Score Improvement has no evidenced retake/reset/eligibility protocol.
  The paired integer `chance_num`/`answer_state` values are now preserved as
  bounded `cidaren_current_question_state` metadata, but remain current-Question
  observations rather than correctness/history/retake authority. A typed
  Provider state/accessor survives fingerprint-bound Question recovery without
  requiring callers to interpret metadata JSON. The values are excluded from
  stable remote Question identity, and a completed Task must not be restarted
  speculatively.

These are not missing Cidaren parsers: the absent wire facts must remain
unknown/unsupported in shared state until a new donor or authorized trace
establishes them.

Cidaren does not register `TaskExecutionCapability`; its assessment lifecycle
is expressed by QuestionInventory plus SubmissionExecute/SubmissionVerify.
Consequently there is no Cidaren TaskExecution completion-diagnosis override.
The SubmissionVerify override uses only fresh Provider-emitted state/progress
tuples. Task-level progress below 100 does not identify which required child is
pending, so it remains undiagnosed. No score-below-threshold,
duration-insufficient, prerequisite, teacher-review, human-action or
attempt-limit diagnosis is inferred.

Core revisions through `1824fb4` now persist completion observations and may
continue an active Strict Completion workflow only for non-stopping verified
diagnoses. They expose no new Cidaren protocol hook. Cidaren's only specific
incomplete diagnosis is `WindowClosed`, while every other unfinished snapshot
falls back to Core `RemoteUnknown`; both stop automatic continuation. The
Provider therefore grants no implicit retry/retake authority. Its donor
`task_type == 2` remains a routine class-test source, not an independently
evidenced Formal assessment classification, and this does not weaken the
separate explicit authorization required for mutations.

The one-time full upstream sweep found no migration omission: the historical
batch/class-vs-study flags compose already complete TaskInventory and durable
per-Task execution; all donor timing semantics are present in runtime schema
revision 2; compression, updater, logs, music and first-run state are transport
or UI concerns; and the old `get_all_task` symbol is an uncalled no-request
stub. Future checkpoints return to normal revision/delta review.

## Frozen donor executable-surface parity

The frozen `a74b4a2` owner donor and `bce9559` current public donor were
re-audited by executable call site rather than GUI labels. Their
platform-facing surface maps as follows:

| Donor call site | Wire operation | Asterism implementation |
|---|---|---|
| `login.verify_token` / `basic_api.get_select_course` | `Student/Main` | authentication validation, selected-Course binding and native OAuth fresh readback |
| `main_api.get_class_task` | `ClassTask/PageTask` | complete bounded native pagination, Course/Task inventory and fresh rediscovery |
| `basic_api.get_all_unit` | `StudyTask/List` | selected-Course ordinary Task inventory and fresh rediscovery |
| `basic_api.get_unit_words` | `StudyTask/Info` | task-bound word inventory for ordinary and self-built flows |
| `basic_api.get_book_all_words` | `Resource/CoursePage/{course_id}.json` | credential-free, origin-locked self-built word inventory |
| `main_api.query_word` | `Course/StudyWordInfo` | bounded meaning, phrase and example evidence |
| `basic_api.use_api_get_prototype` | `Course/SearchWord` | bounded prototype alias resolution |
| `main_api.select_all_word` | `SubmitChoseWord` | signed one-shot word-selection mutation and acknowledgement parsing |
| `main_api.get_exam` | `StartAnswer` | signed non-idempotent attempt start plus legacy/current response decoding; optional `topic_done_num/topic_total` are exposed as an independently bounded remote progress observation |
| `main_api.submit_result` | `VerifyAnswer` | single-answer and sequential rotated-token matching mutations |
| `main_api.next_exam` | `SubmitAnswerAndSave` | signed answer/reading-card advance with stable reported duration |
| `main_api.skip_exam` | `SkipAnswer` | explicit signed skip with stable reported duration |
| `main_api.get_task_score` | post-run `ClassTask/Info` or `StudyTask/Info` score read | exact authenticated Native HTTP route after fresh Task rebinding; `score/task_score/grade` are one conflict-checked fact mapped to Core thousandth-points independently from completion/progress |
| response decoder | fixed-index `jv=2_*`/owner `3_1021`, public span/chunk `3_1021/3_2265/3_2277`, owner `jv=99` | all exact transforms implemented with bounded unique-result semantics and authenticated crypto context for `jv=99` |
| capture helpers | public proxy-observed token-only `UserToken`; owner `UserToken` plus `CDR_LOGIN_INFO` | Both exact recipes, validation and stored-session paths are ordered and registered; actual system-proxy/XWeb acquisition remains a shared helper-execution gap, while OAuth uses the implemented random-marker callback path |

Donor update checks, GUI controls, logging/export, completion sound, the
unrelated Gitee access-log probe and release-only `cdr.660916.xyz`
telemetry/device-block service do not express Cidaren platform protocol
capabilities. The unused Google-translate helper has no source caller and is
not present in the packaged 1.5.4 executable. They are recorded here to make
the audit exhaustive, not treated as deferred Provider abilities.
