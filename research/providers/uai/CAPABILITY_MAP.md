# UAI capability map

| Asterism capability | Primary evidence | Use | Current decision |
|---|---|---|---|
| Authentication | AutoFinish + UnipusHelperPro + current Rust donor | Reference | Password/ImportedToken/AssistedSession orchestration, registry-validated Capture recipe v1 for one-snapshot `u-openid` + raw `Authorization` acquisition, Core-compatible transient ProviderSpecific Password input, strict login-envelope classification, native Password exchange, standard JWT-expiry hint and atomic openid/JWT CompositeSession are offline/native-boundary covered; browser recipe execution remains shared Capture-helper wiring |
| Stored session validation | User-info and Course-list reads | FromScratch | Core provider-scoped resolver accepts only exact unexpired native Composite or validated imported JWT metadata from ManualImport, CaptureTool or BrowserExtension; expired native Composite can use its complete Password set only for atomic user-info-validated renewal, while imported JWT cannot renew |
| CourseInventory | AutoFinish + UnipusHelperPro | Reference | Native authenticated Course list plus bounded Course → CourseResource flattening are offline/native-boundary covered |
| TaskInventory | AutoFinish + UnipusHelperPro | Reference | Native fresh resource-detail/tree reads plus bounded nested Course JSON → Unit/Section/Node/Group parser are offline/native-boundary covered |
| TaskDetail | Asterism Core + both backend donors | FromScratch | Re-lists CourseResources and re-runs fresh detail/tree inventory before returning one exact identity-bound Group detail |
| TaskProgressRead | Both backend donors | Reference | Native fresh-detail + signed per-Unit read and identity-bound Group parser are offline/native-boundary covered; only pass/pass2/perm all 1 maps to completed |
| DurationRead | UnipusHelperPro | Reference | Native user-info + per-Unit study-record read binds CourseResource/Unit/Group exactly; donor-documented Task duration seconds are offline/native-boundary covered |
| Daemon composition | Asterism Core | FromScratch | Complete Development entry is registered only through an independent disabled-by-default opt-in and configured SecretStore |
| QuestionInventory / QuestionParse | AutoFinish + current Rust donor | Reference | Fresh encrypted content read, bounded framing/decryption and typed choice, multi-child composite choice, short-answer, translation/revise, fillblank/banked-cloze, ordering and matching semantics are offline covered; per-child option sets, content-derived judge labels and matching-left order remain bound |
| AnswerResolve | Both backend donors | Reference | A separate fresh encrypted standard-answer read is independently bound to the same Group/question snapshot; ciphertext, keys and route context are not returned |
| SubmissionBuild | Both backend donors | Reference | Credential-free and answer-value-free immutable preview is offline covered; no executable body is retained in the draft |
| SubmissionExecute | Both backend donors + current Rust donor | Reference | Exact `newExploration/submit` mapping accepts bounded ordered typed Groups with one homogeneous type or one type per Question; every positive numeric module identity, answer and multi-module content-derived child judge descriptor stays position-bound, while `600001`/`600002` remain typed retry responses and never become receipts |
| SubmissionVerify | UnipusHelperPro | Reference | Exact receipt-versioned fresh user-module readback independently binds Course/Group/version plus the complete ordered Question set, submitted states and answers; missing receipts are Inconclusive without a guessed route |
| ResourceExecution / ExecutionVerify | UnipusAI + UnipusHelperPro + AutoFinish + Asterism Core | Reference | Five agreed pure-study base labels require a fresh exact `tab_type=text|video` progress leaf; exact `exit-ticket` and each exact single oral family require a fresh `tab_type=task` leaf. Preset/exit-ticket send the donor-observed empty `submitType=2` body, while oral sends its distinct bounded `instanceId=0` placeholder-answer shape; each mutates at most once and uses the Provider's fresh exact progress verify-only path without duration inference or POST replay |
| DurationReport | AutoPlayer | Reference | Active first-batch BrowserBridge path: real rendered Unit/Section/Micro → Tab → Task traversal, bounded page residence, popup handling and optional video playback/keepalive followed by fresh DurationRead; shared browser-execution contract is the current Core Gap, not a deferral |
| Result verification | Fresh tree/progress/duration reads | FromScratch | Require independent readback after any future mutation |
| BrowserBridge | AutoPlayer | Reference | Provider now fresh-rediscovers the exact Group and returns a hashed account/Task isolation key plus exact `ucontent`/`ipub` origins in non-headless mode; exact legacy/Ant/u3menu discovery, Micro→Tab→Task traversal, bounded popup/video handling, pause/restart residence semantics and origin/session-bound iframe messaging are audited, while target/action/residence/session-injection/result orchestration continues through the shared Core Gap |
| Master runtime settings | Asterism Core + AutoPlayer | FromScratch | Versioned Provider schema exposes conservative platform/account execution concurrency, account scan interval, and platform/account/Task overrides for bounded page-residence seconds plus optional video playback; Core remains owner of resolution, scheduling and immutable execution snapshots |
| Discussion | AutoFinish | Reference | Provider-private native boundaries now cover fresh Course/class/curricula/current-user binding, exact topic lookup, bounded reply pagination, zeroizing immutable reply intent, reply mutation receipts and exact current-user/content readback. Registration as a cross-Provider capability and durable ambiguous-recovery orchestration still require the shared immutable discussion Draft contract; final `submitType=2` completion remains a separate verified mutation |
| Exit-ticket / oral empty execution | AutoFinish | Reference | Exit-ticket has a dedicated exact-task empty completion path. Exact `oral-sentence`, `video-dub` and `oral-personal-state` Groups now use the donor's separate bounded placeholder-answer body; all require fresh task-leaf preflight, accepted version receipt and fresh progress verification with no ambiguous replay |
| File upload | AutoFinish | Reference | Provider-private native boundaries now cover freshly bound CMS grant acquisition, zeroizing bounded audio artifact ownership, exact token/key multipart upload, donor minimal-MP3 construction and exact object-store key readback. Final immutable upload-answer submission plus owner/account/Task/Draft-bound durable artifact lifecycle remain a shared contract gap |
| External answer / media transcription | AutoFinish + current Rust donor | Reference | LLM answer generation and audio/video transcription are donor capabilities; integrate as separate external AnswerResolve sources, not as hard-coded UAI protocol logic |

## Current implementation checkpoint

The current crate advertises Authentication, CourseInventory, TaskInventory,
TaskDetail, TaskProgressRead, DurationRead, QuestionInventory/QuestionParse,
AnswerResolve, verified ResourceExecution, the three independent submission
capabilities and the current BrowserBridge session-spec boundary. This is a
checkpoint, not a Provider completion boundary. It:

1. parses a bounded authenticated Course list and flattens each CourseResource
   into a stable `RemoteCourse`;
2. binds a fresh Course-resource detail to the selected resource without
   serializing `courseInstanceId`;
3. decodes the bounded nested `course` JSON string and emits stable Group Tasks
   with separate Unit, Section and Micro hierarchy facts;
4. classifies bounded Password results, separates slider verification as
   `HumanRequired`, and validates Password or strict ImportedToken input from
   ManualImport, CaptureTool and BrowserExtension through Capture recipe v1;
5. stores openid/JWT together as one encrypted ProviderCompositeSession while
   retaining username/password only for future explicit renewal;
6. resolves only the account/reference-bound CompositeSession purpose and
   rejects mismatched origin, kind, expiry, identity or storage shape; captured
   and browser-imported sessions retain their acquisition authority;
7. drops class, instance, content, answer and unknown fields;
8. implements native Password exchange, user-info JWT validation, complete
   Course/Task inventory and per-Unit Group progress;
9. atomically renews only complete NativeProviderLogin+Composite credentials,
   with one Authentication-only retry and no imported JWT renewal;
10. re-lists CourseResources and rebuilds the exact Group from fresh
    detail/tree inventory for every TaskDetail read;
11. preserves bounded `base` task types and optional `question_num` while using
    a versioned Task fingerprint;
12. reads fresh bounded user identity and the exact per-Unit study-record tree,
    then returns seconds only for one unique identity-bound Group Task;
13. advertises ProgressRead and DurationRead on each normalized Group Task so
    Core can authorize the two independent read paths;
14. reads and decrypts fresh content and standard-answer documents separately,
    binds every normalized question and candidate to the exact stable remote
    Group Task snapshot,
    and drops ciphertext, key material and free-form route context;
15. builds only an answer-value-free immutable preview, then forms the native
    submission body inside the execution boundary for bounded ordered choice,
    short-answer, translation/revise, fillblank/banked-cloze, ordering and
    matching and multi-child composite-choice Questions, each with its
    donor-audited positive numeric instance
    identity and exact positional type/pairing facts; every execution requires
    exact bounded content-derived child judge labels and never guesses them
    from the Group base;
16. accepts only a successful version-bearing submit response as a receipt,
    returns ambiguous transport failures after one mutation attempt, and
    independently verifies that exact version through a fresh user-module read;
17. confirms only a complete ordered readback with exact identity, submitted
    state and answer equality for every draft item, returns Inconclusive when no
    version receipt exists, and never infers score, progress or completion;
18. submits the exact empty body for `rich-text-read`, `text-learn`,
    `vocabulary`, `input`, `video-point-read` and the separately classified
    exact `exit-ticket`, while exact single oral families use the donor's
    separate bounded placeholder-answer body; the Provider returns an
    unverified acknowledgement only after fresh progress rebinds the candidate
    to a `text`/`video` leaf for presets or `task` leaf for exit-ticket/oral, while
    its goal-bound verify-only method
    rebinds fresh detail and reads the exact Group progress for Core recovery
    without repeating the mutation;
19. composes these capabilities into an independently opted-in daemon
    Development entry that still requires the configured SecretStore;
20. fresh-rediscovers the exact Group before issuing an account/Task-isolated,
    two-origin non-headless BrowserSessionSpec and advertises BrowserBridge on
    every normalized Group; this spec is not yet the full page-action executor;
21. makes no live compatibility claim until real-account validation, while
    continuing BrowserBridge DurationReport, shared discussion integration,
    upload and external AnswerResolve integration rather than treating this
    checkpoint as an endpoint.

The Provider completion standard is complete migration of every reproducible
audited donor capability. Native HTTP coverage, BrowserSessionSpec coverage or
any other intermediate milestone is only a reportable checkpoint.
