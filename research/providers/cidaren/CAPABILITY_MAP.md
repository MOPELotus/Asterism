# Cidaren capability map

| Asterism capability | Primary evidence | Use | Current decision |
|---|---|---|---|
| Authentication | Current donor + first-party H5/live-safe capture | PortSource + Reference | Manual opaque `UserToken`, captured Composite sessions and the native V2 code exchange are implemented. The native path binds the callback's device code and exact User-Agent/derived `Abc`, generates fresh P-256/SPKI, signs once, performs ECDH/HKDF/AES-GCM, validates the decrypted token through fresh `Student/Main`, and yields the same atomic Composite credential shape. Acquisition/one-shot delivery of the OAuth callback artifact from PC WeChat remains a shared helper/Core gap; Password is not invented without evidence |
| Stored session validation | Current donor + Asterism secrets boundary | FromScratch | Resolver accepts either one exact manual access token or an exact CaptureTool/BrowserExtension Composite token + crypto context, all account/reference/purpose/session bound |
| Session recovery / refresh | Current donor | Reference | No audited refresh endpoint exists; expired sessions require fresh manual import or Capture-assisted reacquisition rather than silent refresh claims |
| CourseInventory | Current/private task routes + public issue fixtures | PortSource | Native signed class pagination and selected-Course `StudyTask/List` are merged by stable `course_id`; conflicting titles fail the complete inventory |
| TaskInventory | Current/private donor + public issue 83 | PortSource | Native `ClassTask/PageTask` pagination is all-or-nothing; class learning/test identities bind to `release_id`, while ordinary study units bind to `course_id + list_id` because `task_id` may remain `-1` |
| TaskDetail | Current donor + Asterism Core | FromScratch | Offline/native-boundary covered: freshly re-list the matching class or study endpoint and require one exact stable identity before returning details; never trust a stale `task_id` |
| TaskProgressRead | Current/private task rows | PortSource | Offline/native-boundary covered for class and ordinary study Tasks: fresh rows expose bounded percent/state plus independently converted duration when present; score and completion remain separate |
| DurationRead | Current donor + public issue 6 | PortSource | Registered and fresh-read for class/study Tasks with `time_spent`: non-zero public rows, millisecond epoch/duration fields in the same envelope and donor mutation values establish millisecond wire semantics; values are bounded to 100 years and truncated only at the Domain seconds boundary |
| QuestionInventory / QuestionParse | Current donor + public issue 43 | PortSource | Legacy and current `jv=99` decoding plus strict single/matching/text/nested-tag parsing are offline-covered; `topic_code` stays ephemeral. The Provider-private attempt machine consumes `StartAnswer` as a one-shot mutation; public slot integration waits only for durable Core AttemptStart/QuestionSession |
| AnswerResolve | Current donor | PortSource | Native task-bound inventory, `StudyWordInfo` and bounded `Course/SearchWord` prototype lookup are implemented. Donor strategies resolve bidirectional meaning, ordered matching, separately-scoped phrase/example evidence and completion/example behavior; audited random/fixed-third/last-word failure fallbacks are retained with stable Draft-safe selection and explicit low confidence, including well-formed empty evidence families. Sentence modes load evidence once per top-level parent, preserve donor ordering, resolve exact nested child wire tags and retain the donor's third-parent fallback |
| SubmissionBuild | Current donor | Reference | Registry-advertised one-current-Question immutable Draft preview is implemented for selection/text/matching; it exposes field names only and never persists topic codes, signatures or endpoints |
| SubmissionExecute | Current donor | PortSource | Native transports plus a one-shot Provider-private state machine cover `SubmitChoseWord`, `StartAnswer`, sequential `VerifyAnswer`, `SubmitAnswerAndSave` and `SkipAnswer`. Issued/ambiguous/failed-closed states prevent replay, matching consumes each rotated token serially, and only a terminal Completed state can emit a bounded receipt for later fresh verification. Durable public execution awaits the shared AttemptStart/QuestionSession slot |
| SubmissionVerify | Fresh class task/detail read | FromScratch | The read-only verifier is implemented and Draft/preview/task bound: a receipt or localized completion message is insufficient, fresh exact release/list completion is required, and per-Question status remains honestly `Unverified` because no answer-history endpoint is evidenced. Registry integration remains paired with durable SubmissionExecute |
| Capture bootstrap | Current donor + PC WeChat XWeb audit | PortSource + Reference | Composite ingestion and the bounded browser-storage recipe are implemented, but the shipped generic helper creates an isolated Edge/Chrome CDP profile and cannot observe the authenticated PC WeChat XWeb callback/storage. Therefore the recipe is a valid credential shape, not an end-to-end WeChat bootstrap claim. The preferred minimal remainder is callback-only XWeb/Capture acquisition followed by the implemented native V2 exchange |
| BrowserBridge | Current donor | PortSource | Provider policy is implemented and advertised for freshly rebound class/study Tasks: visible, account/task hash-isolated and restricted to `https://app.vocabgo.com`. The shared `BrowserSessionSpec` currently carries only isolation/origin/headless policy and no executable start route/action/result contract, so Engine/API execution is a recorded Main-owned gap rather than a completed fallback |

## Current implementation checkpoint

The current checkpoint (not a completion boundary):

1. establish a Development-only Provider crate with exact canonical ID
   `cidaren`;
2. validate bounded, redacted and zeroized manual-token and captured Composite
   sessions;
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
14. advertises Capture-assisted authentication, BrowserBridge and
    SubmissionBuild, and parses current Questions without persisting
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
18. exposes eight provider/account/task runtime settings for scheduler
    concurrency/scan policy and donor answer delay/reported-duration behavior,
    with stable per-step range selection;
19. retains donor failure fallbacks without hiding their quality: random choice
    is stable for an immutable Question, fixed-third and last-word paths remain
    explicit, and all carry low confidence plus sanitized provenance; nested
    sentence options retain their top-level identity so semantic matches use
    the exact child wire tag and the fixed-third failure path uses the exact
    third parent tag rather than the third flattened child;
20. implements the current first-party V2 OAuth code exchange as a one-shot
    Provider-native operation: callback-bound device code and User-Agent/Abc,
    fresh P-256/SPKI, sorted signed request,
    ECDH/HKDF/AES-GCM authenticated response, exact Composite material and
    mandatory fresh account readback, with no ambiguous retry.

The remaining work is durable shared attempt/submission registration, a real
PC WeChat callback-only helper/AuthSession artifact boundary, an executable
BrowserBridge job contract and live validation. Fresh post-mutation
verification is already implemented Provider-side. A checkpoint or Core Gap
is not a Provider stopping condition.

Cidaren is complete only when all audited donor capabilities have current
executable implementation/verification, or every remainder has a concrete
hard blocker that cannot be resolved through further audit, Capture,
BrowserBridge, protocol work or a Main-owned Core change.
