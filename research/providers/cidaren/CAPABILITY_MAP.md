# Cidaren capability map

This map combines the reopened `ularch@bce9559`, owner-supplied
`MOPELotus@a74b4a2` and OAuth V2 assisted-bootstrap handoff. Their additive
differences are frozen in [`DONOR_DIFFERENCES.md`](DONOR_DIFFERENCES.md).

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
| QuestionInventory / QuestionParse | Current donors + public issues 43/99 | PortSource | Fixed-index `jv=2_*`, both evidenced `jv=3_1021` transforms, public `jv=3_2265/3_2277` and current authenticated `jv=99` decoding plus strict single/matching/text/nested-tag parsing are offline-covered; conflicting `3_1021` candidates must yield one unique/equal JSON value. Public issue 99's donor-unsupported mode 73 is now parsed as a bounded two-blank Question with answer-count/word-length agreement, allowing the evidenced Skip path without guessing its unknown multi-answer wire encoding. The rotating `topic_code` remains absent from Domain Questions and Drafts; a stable Provider-private artifact now binds it to local/remote Task, Question ID, position and content fingerprint for encrypted Core storage. Public-donor `topic_done_num/topic_total` are retained as an optional bounded remote progress observation distinct from local attempt position. The Provider-private attempt machine consumes `StartAnswer` as a one-shot mutation; public slot integration waits only for durable Core AttemptStart/QuestionSession |
| AnswerResolve | Current donor | PortSource | Native task-bound inventory, `StudyWordInfo` and bounded `Course/SearchWord` prototype lookup are implemented and registered as a public capability. Donor strategies resolve bidirectional meaning, ordered matching, separately-scoped phrase/example evidence and completion/example behavior; audited random/fixed-third/last-word failure fallbacks are retained with stable Draft-safe selection and explicit low confidence, including well-formed empty evidence families. Sentence modes load evidence once per top-level parent, preserve donor ordering, resolve exact nested child wire tags and retain the donor's third-parent fallback |
| SubmissionBuild | Current donor | Reference | Registry-advertised one-current-Question immutable Draft preview is implemented for selection/text/matching; it exposes field names only and never persists topic codes, signatures or endpoints |
| SubmissionExecute | Current donor | PortSource | Native transports plus a one-shot Provider-private state machine cover `SubmitChoseWord`, `StartAnswer`, sequential `VerifyAnswer`, `SubmitAnswerAndSave` and `SkipAnswer`. Issued/ambiguous/failed-closed states prevent replay, matching consumes each rotated token serially, verified-answer and reading-card advance commands carry distinct stable donor pre-execution residence times, and only a terminal Completed state can emit a bounded receipt for later fresh verification. Durable public execution awaits the shared AttemptStart/QuestionSession slot |
| SubmissionVerify | Fresh class task/detail read + current public donor score read | FromScratch | The read-only verifier is implemented and registered independently of mutation execution. It is Draft/preview/task bound: a receipt or localized completion message is insufficient, fresh exact release/list completion is required, and the independently observed bounded 0-100 score is converted to Core thousandth-points when present. Per-Question status remains honestly `Unverified` because no answer-history endpoint is evidenced. SubmissionExecute remains paired with durable Core Attempt integration |
| Capture bootstrap | Current donors + PC WeChat XWeb audit | PortSource + Reference | Token-only and Composite ingestion plus separate bounded token-only request-header and Composite browser-storage recipes are implemented and advertised as ordered alternatives. Core freezes one exact version per bootstrap and never mixes their outputs. Cidaren validates a typed Provider-private `CaptureSnapshot` command/result for those recipes, including exact origin/frame/Task/sequence binding and `jv=99` context parsing, then converts only an exact token-only or atomic token+crypto replacement for the shared credential commit; the donor-observed but unused `CDR_USER_SESSION` is not persisted. The generic helper's isolated Edge/Chrome profile still cannot observe an authenticated PC WeChat XWeb storage context, so actual PC WeChat/system-proxy helper execution remains shared work rather than a false end-to-end claim. OAuth bootstrap no longer depends on XWeb/MITM: users can return the preserved random-marker callback URL from an audited WeChat device flow |
| BrowserBridge | Current donor | PortSource | Provider policy is implemented and advertised for freshly rebound class/study Tasks: visible, account/task hash-isolated and restricted to `https://app.vocabgo.com`. The Provider has a typed CaptureSnapshot command/result boundary (TokenOnly v1 and Composite v2), fresh-Task command construction/result parsing and an immutable adapter to Core's durable one-shot ledger. It derives the command nonce from the exact Core session ID, freezes type/digest/sequence at issue, repeats fresh rebinding before acceptance and returns only zeroizing Provider-private Capture material plus terminal hash metadata. Core persists contiguous sequencing and idempotent result retry; public helper payload transport/dispatch and Capture credential commit remain shared gaps, so no executable start/action/result fallback is claimed yet. Cidaren assessment mutations remain native HTTP because no donor browser action for those routes was evidenced |

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
   TaskDetail, TaskProgressRead and SubmissionBuild slots into one
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
    SubmissionBuild and SubmissionVerify, and parses current Questions without persisting
    `topic_code`;
15. freezes fresh binding and exact request/signature vectors for
    `StartAnswer`, `VerifyAnswer`, `SubmitAnswerAndSave`, `SkipAnswer` and
    `SubmitChoseWord`;
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
    versioned, zeroizing Provider artifact whose digest is stable and whose
    decode path rebinds the local Task, remote Task, remote Question, position
    and complete Question content fingerprint. The plaintext is intended only
    for Core's encrypted QuestionSession continuation store and never enters a
    normalized Question, immutable Draft or diagnostic output.
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
    immutable Question binding across every topic-code revision.
32. encodes pre-Question selection/start/reading-card state as the bounded
    `cidaren.pre-question-attempt.v1` artifact. Recovery rebinds local/remote
    Task identity, requires a fresh word-selection plan instead of persisting
    its large map, restores reading-card progress and allows correlation to
    change because it remains audit metadata rather than execution identity.

The remaining work is durable shared QuestionSession/Attempt registration for
QuestionInventory/QuestionParse and SubmissionExecute, shared execution for
the alternate Capture/BrowserBridge helper paths and live validation.
Fresh post-mutation verification and the durable one-shot External OAuth path
are already implemented. A checkpoint or Core Gap is not a Provider stopping
condition.

Cidaren is complete only when all audited donor capabilities have current
executable implementation/verification, or every remainder has a concrete
hard blocker that cannot be resolved through further audit, Capture,
BrowserBridge, protocol work or a Main-owned Core change.

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

Donor update checks, GUI controls, logging/export, completion sound and the
unrelated Gitee access-log probe do not express Cidaren platform protocol
capabilities. The unused Google-translate helper has no executable caller.
They are recorded here to make the audit exhaustive, not treated as deferred
Provider abilities.
