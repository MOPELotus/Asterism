# UAI capability map

| Asterism capability | Primary evidence | Use | Current decision |
|---|---|---|---|
| Authentication | AutoFinish + UnipusHelperPro + current Rust donor | Reference | Password/ImportedToken/AssistedSession orchestration, registry-validated Capture recipe v4 for same-snapshot `u-openid` + raw `Authorization`, optional donor `u-school` and an exact `ucontent` Cookie header, separate navigation/read origins and required-output readiness, Core-compatible transient ProviderSpecific Password input, strict login-envelope classification, native Password exchange, standard JWT-expiry hint and atomic openid/JWT/optional-school CompositeSession are offline/native-boundary covered; browser recipe execution remains shared Capture-helper wiring |
| Stored session validation | User-info and Course-list reads | FromScratch | Core provider-scoped resolver accepts only exact unexpired native Composite or validated imported JWT metadata from ManualImport, CaptureTool or BrowserExtension and optionally resolves the separately purpose-bound captured Cookie; the Cookie is injected only into `ucontent` requests with the donor client headers and never leaks to `uai`/`ucloud`. Expired native Composite can use its complete Password set only for atomic user-info-validated renewal, while imported JWT cannot renew |
| CourseInventory | AutoFinish + UnipusHelperPro | Reference | Native authenticated Course list plus bounded Course → CourseResource flattening are offline/native-boundary covered |
| TaskInventory | AutoFinish + UnipusHelperPro + current Rust donor | Reference | Native fresh resource-detail/tree reads, the current donor's independent Course-progress read and one signed progress read for every exact Unit are merged all-or-nothing. Course progress must repeat the exact tree Unit set, independently supplies/cross-checks the positive current publish version and retains per-Unit strategy facts; every per-Unit response must also echo the exact account, Course instance and Unit, and its optional publish version must agree. The bounded Unit/Section/Node/Group parser separately retains fresh per-Group required, minimum-score, statistic-mode, tab-type and availability-window facts, maps only pass/pass2/perm all 1 to completed and fingerprints the enriched Task snapshot |
| TaskDetail | Asterism Core + backend donors | FromScratch | Re-lists CourseResources and re-runs fresh detail/tree plus every-Unit progress inventory before returning one exact identity-bound Group detail |
| TaskProgressRead | Backend donors + current Rust donor | Reference | Native fresh-detail + signed per-Unit read and identity-bound Group parser are offline/native-boundary covered; the native boundary requires exact `open_id`/`tutorialId`/`unit_id` echoes and a valid optional publish version before returning the redacted document. pass/pass2/perm, tab type and donor strategy facts remain separate, only all three completion flags map to completed, and nonzero start/end define the strict mutation window |
| DurationRead / Task study record | UnipusHelperPro | Reference | Native user-info + per-Unit `unitTaskSituation` read binds CourseResource/Unit/Group exactly. Its Provider-private snapshot preserves bounded Task `finishProgress`, donor-documented duration seconds, optional `required`, `scoreTaskFlag` and `taskQuesTotalScore`; shared DurationRead projects only seconds instead of discarding or conflating the other independent facts. Shared exposure for the richer Task study record remains a separate Core contract item |
| Course/Unit aggregate progress | UnipusHelperPro | Reference | Provider-private bounded parser and native transport cover the independent `totalAndUnitSituation` Course total plus unique Unit finish-progress, learning-seconds, score and required facts, bound to fresh app-user and CourseResource identities. Shared Course/Unit aggregate capability, Scheduler and API exposure are an accepted Core Gap; these semantics are not forced into TaskProgressRead or DurationRead |
| Course/Unit required policy | UnipusHelperPro | Reference | Provider-private bounded parser and native POST cover `courseStudyStrategy/detail` after fresh detail binds the CourseResource and strategy ID. It preserves Course unlock/scoring/time facts plus ordered unique Unit strategies, required Task IDs, pass-score/score-type and Unit windows; it remains distinct from per-leaf progress `required` and awaits the shared aggregate/policy contract |
| Daemon composition | Asterism Core | FromScratch | Complete Development entry is registered only through an independent disabled-by-default opt-in and configured SecretStore |
| QuestionInventory / QuestionParse | AutoFinish + UnipusHelperPro + current Rust donor | Reference | Fresh encrypted content read, bounded framing/decryption and typed choice, multi-child composite choice, short-answer, writing, translation/revise, fillblank/banked-cloze, ordering and matching semantics are offline covered. The donor `video-popup` media container is also covered without a guessed fixed kind: its fresh module/child `replyType` derives choice, composite-choice or ordered text-blank semantics and incompatible mixtures fail closed. Per-child option sets, content-derived judge labels and matching-left order remain bound. Donor media paths/subtitle tracks and embedded WEBVTT are also parsed: bounded WEBVTT normalization removes headers, cue ordinals and timestamps before retaining transcript text. The complete cached reference set is consumed atomically into one immutable Question set. When any Question has media, a single encrypted Core continuation binds every ordered Question's Task/remote identity, position and content fingerprint plus each media attachment ID, kind, canonical route and subtitle flag; artifact-free sets return `None` and keep the legacy path |
| AnswerResolve | Both backend donors | Reference | A separate fresh encrypted standard-answer read is independently bound to the same Group/question snapshot; ciphertext and keys are not returned. Core's claimed QuestionSession continuation is optional: when present, UAI requires the exact Provider type/phase/revision/digest, decodes and rebinds the complete Question manifest before any network read, then fresh-rechecks Task/content and resolves standard answers. Invalid/foreign media artifacts fail before transport, while no-artifact callers retain the original resolver |
| SubmissionBuild | Both backend donors | Reference | Credential-free and answer-value-free immutable preview is offline covered; no executable body is retained in the draft |
| SubmissionExecute | Both backend donors + current Rust donor | Reference | Exact `newExploration/submit` mapping accepts bounded ordered typed Groups with one homogeneous type or one type per Question; every positive numeric module identity, answer and multi-module content-derived child judge descriptor stays position-bound. Before POST, the native transport refreshes detail plus the exact Unit progress, requires an incomplete `tab_type=task` leaf inside its donor availability window and binds the exact Unit/Group. Answer-bearing `video-popup` retains its content-derived children but uses the independently donor-evidenced study-mode `submitType=2`; a mixed ordinary/video Group remains type 1. Native Task inventory now always obtains the current positive Course `publish_version` from the independent Course-progress route and requires any tree copy to agree before binding each judge to `{course: publish_version, answer: 3}`. The independently evidenced MIT `{course: 0, answer: 0}` pair remains only for explicitly injected legacy fixture transports that omit Course progress. After fresh Course/account rebinding, a redacted zeroizing request owner binds the exact POST URL, content type and complete body into a stable pre-dispatch digest, and Native HTTP sends those same owned bytes. An artifact-bearing QuestionSession additionally requires the exact complete UAI type/phase/nonzero-revision/digest manifest before fresh planning; the native transport freezes its session, time-bound annotator token and exact request into `uai.answer-submit.v1`, Core persists the request digest before dispatch, and a Provider/account-bound wrapper rejects foreign execution contexts before one accepted response supplies a separate response digest plus receipt. Ambiguity recovery revalidates the same session/operation metadata and freshly rematerializes the read-only request; drift fails closed, while an exact match stays locked and returns no replay because the receipt version cannot be recovered safely. Foreign artifacts fail before transport, and media-free callers retain the legacy single-call path. `600001`/`600002` remain typed retry responses and never become receipts |
| SubmissionVerify | UnipusHelperPro | Reference | Exact receipt-versioned fresh user-module readback independently binds Course/Group/version plus the complete ordered Question set, submitted states and answers. Session-aware verification first requires and decodes the same exact complete UAI media type/phase/nonzero-revision/digest manifest against the immutable Draft, so a foreign continuation fails before Task or user-module transport; media-free execution retains the legacy verifier. When the same bounded readback summary identifies donor-counted `questionType=1|3`, its `score_avg` becomes a validated fixed-point `0..=100` score; a real zero is preserved for policy/alerting rather than treated as absence. Uncounted or absent summaries yield no score, and missing receipts are Inconclusive without a guessed route |
| ResourceExecution / ExecutionVerify | UnipusAI + UnipusHelperPro + AutoFinish + Asterism Core | Reference | Five agreed pure-study base labels require a fresh exact `tab_type=text|video` progress leaf; exact `exit-ticket`, exact single `discussion`, exact homogeneous `short_answer`, and each exact single oral family require a fresh `tab_type=task` leaf. Every mutation also requires the fresh donor strategy window to be open. Pure-study execution freezes Master-resolved `marker|placeholder`: marker uses the MIT/current donor no-Question `submitType=2` body, while placeholder reproduces Apache's `instanceId=0` empty child/score-map and base-list padding/truncation into ordered judges with fresh Course version. Exit-ticket uses its exact marker. Direct discussion independently freezes the same two choices, with a homogeneous discussion placeholder. Exact homogeneous `short_answer` independently exposes Apache's `SUBJECTIVE_ALLOW_EMPTY` placeholder execution while retaining its ordinary SubmissionExecute/SubmissionVerify path. Oral uses its bounded placeholder-answer shape and fresh-version rule. After every fresh preflight, all seven marker/placeholder families now pass their exact common POST URL, content type and complete zeroizing body through the same redacted request owner/digest boundary as ordinary answer submission. Each ResourceExecution mutates at most once and uses the Provider's fresh exact progress verify-only path without duration inference or POST replay; persisting that exact digest before TaskExecution dispatch remains a shared Core contract gap |
| DurationReport | AutoPlayer | Reference | Active first-batch BrowserBridge path: real rendered Unit/Section/Micro → Tab → Task traversal, bounded page residence, popup handling and optional video playback/keepalive followed by fresh DurationRead; shared browser-execution contract is the current Core Gap, not a deferral |
| Result verification | Fresh tree/progress/duration reads | FromScratch | Require independent readback after any future mutation |
| BrowserBridge | AutoPlayer | Reference | Provider fresh-rediscovers the exact Group and returns a hashed account/Task isolation key plus exact `ucontent`/`ipub` origins in non-headless mode. Its private plan derives the secret-free HTTPS start route `/_explorationpc_default/pc.html?cid={fresh CourseResource}` after checking Course/Group identity consistency, then rebinds the normalized Unit/Section/Micro/Task labels and freezes legacy/Ant/ARIA/u3menu discovery, exact Tab/Task/popup/video selectors, DOM/action/cardinality/time bounds, master-resolved residence/video settings and mandatory session-nonce+frame+exact-origin messaging. The donor's wildcard `SCAN`/`CLICK`/`PING` channel is mapped to typed correlated commands/events: transport-observed origin must equal the exact envelope origin, nonces are zeroized/redacted, discovered labels are bounded, and clicks accept only the unique fresh hierarchy match through an opaque session-bound handle rather than arbitrary selectors/DOM paths. The same protocol now enumerates bounded ordered Tab/Task snapshots and derives their handles from the same binding; Tab traversal may select a validated snapshot item, while Task selection must uniquely match the fresh target title. A Tab may contain up to the frozen 128 Task entries, while only one exact target Task wrapper can construct the final residence action; that action freezes residence/video settings and correlates the result to both command sequence and target handle. The result parser permits at most the one bound Micro and one target Task per processed Tab, bounds total observed Tasks by processed Tabs × 128, rejects foreign session/frame/origin/Task/command, count/time overflow and unexpected video activity, and still requires fresh DurationRead. Stable command/event/residence-result types and bounded SHA-256 helpers feed Core's durable exchange ledger without persisting Provider-private JSON. Every issued command can become a bounded encrypted-at-rest artifact whose digest equals the ledger identity; recovery decodes it only after rechecking the fresh plan, exact nonce/origin/frame/Task/sequence and schema, so digest-only state never invents a replacement command. Intermediate event and terminal residence documents also have distinct bounded zeroizing/redacted owners which derive the exact ledger result digest before typed parsing. Session injection, actual DOM action dispatch and pause/recovery orchestration continue through the shared Core Gap |
| Master runtime settings | Asterism Core + AutoPlayer | FromScratch | Versioned Provider schema v3 exposes conservative platform/account execution concurrency, account scan interval, and platform/account/Task overrides for bounded page-residence seconds, optional video playback, pure-study `marker|placeholder` and direct-discussion `marker|placeholder` modes; Core remains owner of resolution, scheduling and immutable execution snapshots |
| Discussion | AutoFinish | Reference | Exact single-discussion Tasks expose both donor direct-empty ResourceExecution shapes independently from the richer reply flow: a frozen runtime choice selects `submitType=2` no-Question marker or `submitType=1` `instanceId=0` placeholder with fresh Course judge version. Both fresh-rebind the exact task leaf, mutate once and require fresh progress verification. Provider-private reply boundaries additionally cover fresh Course/class/curricula/current-user binding, exact topic lookup, bounded route-topic-bound reply pagination, zeroizing immutable reply intent, reply mutation receipt and exact current-user/content readback. The first mutation exposes a stable request digest over all route/account/topic/content facts for pre-dispatch registration. Only exact reply readback plus one fresh single-discussion Task can freeze the separate Group-completion mutation, whose digest binds Task fingerprint/hierarchy/topic and verified reply digest before sending the empty marker body. Reply and Group receipts remain independent and neither claims completion; fresh TaskProgressRead remains final. Cross-Provider registration and durable recovery still require the shared immutable discussion Draft/attempt contract |
| Exit-ticket / oral execution | AutoFinish | Reference | Exit-ticket has a dedicated exact-task empty completion path. Exact single `oral-sentence`, `video-dub` and `oral-personal-state` Groups use the donor's separate bounded placeholder-answer body. The audited ordered `basic-scoop-content,oral-sentence` Group has a Provider-private atomic plan: it joins one immutable matching sub-Draft with the same Group's fresh encrypted oral instance and bounded per-child value/`answersExtra` evidence, maps `answersExtra` to the submitted child `extra`, submits both modules once, and verifies every value/extra through receipt-authorized user-module readback. After fresh route/account rebinding, its zeroizing request owner exposes a stable digest over exact POST URL, content type and complete body while redacting all wire material; this request identity stays distinct from the semantic plan fingerprint. Empty scalar/array and donor-stable non-empty text/list/number/bool judge shapes are covered; object values without a stable donor judge encoding fail closed. All paths require fresh task-leaf preflight, accepted version receipts and fresh progress verification with no ambiguous replay. Shared compound Draft/Attempt exposure remains a Core Gap |
| File upload | AutoFinish | Reference | Provider-private native boundaries cover fresh exact Task-detail upload intent, CMS grant acquisition, bounded multipart object upload, final single-`multiFileUpload` mutation and receipt-versioned user-module readback. Task-tree parsing preserves that donor-canonical camel-case label instead of lowercasing it past the upload gates. Task fingerprint, Course/Unit/Group, unique module position, object key, artifact digest and intent fingerprint remain bound across all stages. Each freshly materialized HTTP stage now owns its exact pre-dispatch digest: CMS grant binds GET plus account/Course query and Referer, Qiniu binds POST/origin/content-type plus the complete token/key/artifact multipart body, and both single and compound final submit bind POST URL/content type plus the complete zeroizing account/Course/body document. Semantic fingerprints remain separate from these network identities. The mixed path atomically joins one validated ordinary sub-draft plus the uploaded artifact and verifies both ordered values. Neither receipt/readback claims completion without fresh TaskProgressRead. Durable digest registration, artifact lifecycle, one shared compound Draft/Attempt and capability exposure remain Core gaps |
| External answer / media transcription | AutoFinish + current Rust donor | Reference | LLM answer generation and audio/video transcription are donor capabilities; UAI supplies bounded opaque audio/video/subtitle attachments, embedded transcript semantics and exact HTTPS media sources without placing fetch URLs in Domain Questions. Core now persists the optional complete-set artifact atomically with the immutable snapshot and passes its claimed decrypted value only to session-aware AnswerResolve. UAI recomputes every attachment ID and rejects digest, Task, Question, URL, kind, order, manifest and schema drift before exposing a bound source. A safe downloader still requires shared DNS/private-range and redirect validation plus host-scoped authentication; model credential, transcription and external answer orchestration remain Core gaps |

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
   ManualImport, CaptureTool and BrowserExtension through Capture recipe v4;
5. stores openid/JWT together as one encrypted ProviderCompositeSession and a
   optional `u-school` in that bounded composite, and a captured browser Cookie
   as a separate purpose-bound secret while retaining username/password only
   for future explicit renewal;
6. resolves only the account/reference-bound CompositeSession purpose and
   rejects mismatched origin, kind, expiry, identity or storage shape; captured
   and browser-imported sessions retain their acquisition authority;
7. drops class, instance, content, answer and unknown fields;
8. implements native Password exchange, user-info JWT validation, complete
   Course inventory, and Task inventory enriched all-or-nothing by one fresh
   Course-progress snapshot plus one progress document for every exact Unit;
   the Course snapshot must match the tree Unit set and independently
   supplies/cross-checks the current publish version; each Group retains required,
   minimum-score, statistic-mode, tab-type, completion and availability-window
   facts in its versioned fingerprint;
9. atomically renews only complete NativeProviderLogin+Composite credentials,
   with one Authentication-only retry and no imported JWT renewal;
10. re-lists CourseResources and rebuilds the exact Group from fresh
    detail/tree plus every-Unit progress inventory for every TaskDetail read;
11. preserves bounded `base` task types, optional `question_num`, the
    independently refreshed positive numeric/string Course `publish_version`
    and separate Course-Unit strategy facts while using a versioned Task
    fingerprint;
12. reads fresh bounded user identity and the exact per-Unit study-record tree,
    preserves one unique Group's completion percentage, seconds, required and
    scoring facts in a Provider-private Task record, then projects only seconds
    through shared DurationRead;
13. advertises ProgressRead and DurationRead on each normalized Group Task so
    Core can authorize the two independent read paths; separately implements
    the donor Course/Unit aggregate study-record parser and native transport
    with exact CourseResource/app-user binding while its shared capability
    contract is added by Main; also refreshes Course detail before the separate
    required-policy POST and binds its response to the exact CourseResource and
    strategy ID without replacing progress-leaf required facts;
14. reads and decrypts fresh content and standard-answer documents separately,
    binds every normalized question and candidate to the exact stable remote
    Group Task snapshot, atomically aggregates any media routes into one
    complete-set Core continuation, and requires session-aware AnswerResolve to
    rebind that encrypted manifest before fresh reads; ciphertext, key material
    and free-form route context are dropped;
15. builds only an answer-value-free immutable preview, then forms the native
    submission body inside the execution boundary for bounded ordered choice,
    short-answer/writing, translation/revise, fillblank/banked-cloze, ordering,
    matching, multi-child composite-choice and content-derived `video-popup`
    Questions, each with its
    donor-audited positive numeric instance identity and exact positional
    type/pairing facts; every execution requires exact bounded content-derived
    child judge labels, never guesses them from the Group base, and binds judge
    protocol versions to the fresh Course publish version when available while
    retaining the separately evidenced legacy `0/0` shape when it is absent;
    answer-bearing `video-popup` plans retain their native children while
    selecting the donor study-mode `submitType=2` only when every item has that
    exact base; native execution materializes one redacted zeroizing request
    whose digest binds the exact POST URL, content type and complete fresh
    Course/account/body bytes before dispatch;
16. accepts only a successful version-bearing submit response as a receipt,
    returns ambiguous transport failures after one mutation attempt, and
    independently verifies that exact version through a fresh user-module read;
17. confirms only a complete ordered readback with exact identity, submitted
    state and answer equality for every draft item, returns Inconclusive when no
    version receipt exists, exposes score only from a donor-counted summary in
    that same fresh bound readback, and never infers progress or completion;
18. freezes marker or Apache placeholder mode for `rich-text-read`, `text-learn`,
    `vocabulary`, `input` and `video-point-read`; uses the exact marker for the
    separately classified `exit-ticket`; independently freezes marker or
    placeholder mode for exact single discussion; exposes Apache's
    `SUBJECTIVE_ALLOW_EMPTY` placeholder as a separate ResourceExecution for exact
    `short_answer` without removing its ordinary Submission capabilities; and uses
    the donor's bounded placeholder-answer body for exact single oral families. The Provider returns an
    unverified acknowledgement only after fresh progress rebinds the candidate
    to a `text`/`video` leaf for presets or `task` leaf for
    exit-ticket/discussion/subjective/oral and
    confirms that the donor availability window is currently open, while
    its goal-bound verify-only method
    rebinds fresh detail and reads the exact Group progress for Core recovery
    without repeating the mutation; ordinary answer submission likewise binds
    the exact Unit/Group to a fresh incomplete `task` leaf inside that window
    before building and sending its single POST;
19. composes these capabilities into an independently opted-in daemon
    Development entry that still requires the configured SecretStore;
20. fresh-rediscovers the exact Group before issuing an account/Task-isolated,
    two-origin non-headless BrowserSessionSpec and advertises BrowserBridge on
    every normalized Group; a separate versioned residence plan freezes exact
    donor selectors, discovery families, action/time/cardinality ceilings,
    master-resolved settings and nonce/frame/origin security, while shared
    session injection/action dispatch remains the executor gap;
21. makes no live compatibility claim until real-account validation, while
    continuing BrowserBridge DurationReport, shared discussion integration,
    upload and external AnswerResolve integration rather than treating this
    checkpoint as an endpoint.

The Provider completion standard is complete migration of every reproducible
audited donor capability. Native HTTP coverage, BrowserSessionSpec coverage or
any other intermediate milestone is only a reportable checkpoint.
