# UAI drift register

| Risk | Static evidence | Required response |
|---|---|---|
| Login result/code shape changes | Current donors use string code `0`; captcha uses `1506` | Parse typed bounded envelopes; classify challenges as `HumanRequired` and never log bodies |
| JWT header format changes | Donors pass the returned JWT directly as `Authorization` | Validate through user-info/Course reads; do not guess a Bearer prefix |
| Login/user-info routes redirect or return HTML | Shared client does not follow redirects and both routes are audited JSON APIs | Classify redirects/404 as protocol drift and reject missing/non-JSON Content-Type before parsing |
| Read responses omit or change their success envelope | Audited UAI management APIs require numeric code `1` plus `success=true`; ucontent tree/progress/content require numeric code `0` | Require each route's exact success markers before consuming nested data; never accept a merely well-shaped body |
| JWT/openid are stored or renewed separately | Both values are required by different authenticated routes | Persist one strict ProviderCompositeSession and replace it atomically |
| Core rejects a native Password candidate before Provider validation | Password input uses a transient `ProviderSpecific` bundle but successful validation persists a `Composite` session | Declare `ProviderSpecific`, `Composite` and `Jwt` in Provider metadata; keep the candidate and replacement kinds distinct and require both to pass Core's metadata gate |
| Imported/native session metadata is confused | Both paths store the same encrypted document with different lifecycle authority | Bind ManualImport to Jwt and NativeProviderLogin to Composite; reject every other pairing |
| Concurrent renewals overwrite a fresher session | Password credentials and composite session form one lifecycle set | Serialize by account, validate the new JWT, then compare-and-replace all three credentials atomically |
| JWT expiry is ignored or trusted as authentication | UAI supplies a JWT but user-info remains the native validation authority | Decode standard `exp` only as an early lifecycle/cache bound; never derive identity or validity from unsigned local claims |
| Expired JWT metadata prevents Password renewal | Core persists the same native-session expiry on the complete renewable credential set | Permit expired native Composite metadata only inside the exact username/password/composite renewal path, then user-info validate and atomically replace it |
| Unverified UAI becomes active accidentally | Native boundaries exist but have not passed live validation | Keep daemon registration disabled by default, require an independent explicit opt-in plus SecretStore and emit a Development warning |
| Course list and detail disagree on instance fields | List has `instanceId`; detail has `courseInstanceId` | Use stable CourseResource ID and refresh/bind detail before tree reads |
| Course tree outer envelope changes | Current response stores JSON text in `course` | Bound both layers and fail closed on non-string, malformed or oversized nested data |
| New tree role appears | Audited roles are Unit, Section, Node, Link and Group | Reject unknown roles and add a sanitized fixture before mapping them |
| Group ID disappears or becomes URL-only | Historical parser falls back to URLs for generic nodes | Require explicit bounded Group IDs; never persist route URLs as Task identity |
| Persisted Group facts become stale before a read | CourseResource detail supplies a fresh instance route and trees can change | TaskDetail re-lists CourseResources and rebuilds the exact Group; missing facts become RemoteChanged |
| Task-detail fingerprints change shape silently | Core requires versioned fingerprints for fresh detail validation | Prefix normalized Group hashes with `v1:` and bump the version when fingerprint material changes |
| Completion flags gain new values | Current donor requires `pass == pass2 == perm == 1` | Keep each flag independent and map only sanitized verified combinations |
| Annotator token claims/signature change | Both backend donors independently use the same HS256 openid claims and millisecond expiry across content, progress and submission routes | Keep generation private to the native transport, pin an exact-time vector and fail closed on header/clock errors |
| Native submit unexpectedly depends on an SSO browser cookie or browser-only headers | The MIT donor submits through its login-populated cookie jar, the Apache donor keeps session cookies and JWT defaults, and the current Rust donor imports cookie, openid/CSRF, app/platform and school headers; none proves JWT-plus-annotator alone in a clean client | Keep submission at Development, send only the bounded JWT/annotator contract, and require separately authorized live mutation evidence before adding any cookie or browser header credential |
| Study-record duration unit/meaning drifts | Frozen MIT donor explicitly documents Course/Unit/Task `duration` in seconds | Keep the study-record route independent, bind exact CourseResource/Unit/Group identities and recheck with live before/after measurements |
| Progress duration is confused with study duration | Signed progress response also exposes an untyped numeric `duration` | Preserve progress value as raw only; populate seconds exclusively from the independent study-record response |
| Study-record tree duplicates or changes roles | Donor indexes nodeId and currently emits Unit/Section/Node/Link hierarchy | Require unique bounded node IDs, audited roles and one exact matching Task; reject drift before returning duration |
| Page residence stops producing duration | Current userscript relies on active page lifecycle | Prefer native network evidence; otherwise isolate a headless compatibility worker and verify fresh duration |
| Direct submit completes without duration | Backend donors submit answers independently of page residence | Keep CompletionService and DurationService separate; never claim execution from completion alone |
| Preset completion is forced through ordinary retrying TaskExecution | Current Rust and MIT donors mark study resources with a no-Question `submitType=2` POST, while `SubmissionDraft` requires at least one Question | Advertise ResourceExecution only with ExecutionVerify+ProgressRead, execute once, and let Core persist the attempt and recover through exact Group progress without replaying the POST |
| Preset labels select the wrong mutation shape | The Apache donor's active generic path inserts `instanceId=0` placeholders, while the current Rust donor uses the empty body only for fresh `tab_type=text|video` leaves | Treat the five base labels as candidates only; before any mutation refresh exact Unit/Group progress, require text/video, skip already-completed leaves and reject task/missing/unknown tab types |
| Encrypted content/answer framing changes | Donors currently use `unipus.` hex AES-ECB envelopes with a key suffix | Bound and validate every layer before decrypting; zeroize decrypted bytes on success and failure, recursively zeroize raw parsed string values, and never persist ciphertext, key material or route context |
| Group `base` cardinality no longer describes its Questions | Donors use either one homogeneous type or one type per Question | Require a bounded positive `question_num` plus exactly one supported type or an exact type-to-count cardinality before advertising or parsing Questions |
| Multi-Question module order or cardinality drifts | Current Rust and MIT donors serialize one ordered `quesDatas` row per native module and flatten child completion/judge rows in that same order | Require immutable draft positions `1..n`, exact fresh `question_num`/type cardinality, unique positive numeric module IDs and an exact ordered user-module readback before confirming any Question |
| Multi-Question client-authored metadata diverges | Current Rust donor uses minimal `contextVersion=1`/`answerVersion=1`; MIT/Apache multi builders use `1/0` plus locally fabricated course-answer score maps, while the MIT single builder uses minimal `0/0` | Retain the proven minimal `0/0` single body, use the current donor's minimal `1/1` multi body, never fabricate score/courseAnswer state, and keep both Development until separately authorized mutation validation |
| Multi-Question judge labels are guessed from Group `base` | Current Rust donor copies each decrypted content child/module `type` and child `replyType`; those labels can differ from the tree base | Preserve only bounded sanitized judge labels during answer-free parsing, require an exact label per answer child before multi-module mutation, and fail before POST when content lacks that current contract |
| Missing or non-native Question ID reaches a mutation | Standard-answer and both donors' submission protocols use the positive numeric answer-entry instance ID | Require an explicit bounded remote Question ID for reads and a positive numeric instance ID for execution; never submit a Group/position fallback |
| A Question snapshot is reused against another Group route | Native Question IDs and option labels are not sufficient to prove the CourseResource/Unit/Group target | Persist the stable remote Task identity in sanitized Question metadata and require an exact match before answer resolution or draft construction |
| Submission throttling is mistaken for success | Donor returns `600001`/`600002` for rate limiting | Return typed retry state and never repeat a mutation without Core idempotency/recovery authority |
| An ambiguous mutation failure is retried inside the Provider | A timeout or body failure cannot prove whether the server accepted the request | Return the first Network error after one mutation attempt so Core enters receipt-independent verify-only recovery and never resubmits |
| Submit version cannot safely identify one user-module path | Submit acknowledgement supplies the version later combined with Group identity | Accept only a bounded ASCII path-component version before persisting the receipt; verification reuses the same validator |
| Submit receipt is accepted as result verification | Fresh user-module and progress reads are separate donor calls | Keep SubmissionExecute and SubmissionVerify separate and require a fresh identity-bound readback |
| A missing receipt causes verification to guess the current module version | The readback route is keyed by the submit response version | Return Inconclusive without transport I/O when no accepted version receipt exists |
| User-module route and response identify different Courses or Groups | The route uses fresh `courseInstanceId` while response and state carry separate module/submit metadata | Bind fresh Course detail, top-level and submit-info Course, exact Group-version module, submit-info Group and both version fields before inspecting answers |
| User-module answer is stale, partial or changed | `quesData` embeds separately encoded context and answer children | Require the complete exact ordered Question set, submitted context for every row, every child `isDone`, and exact immutable draft-answer equality |
| Unsupported UAI task kinds acquire a generic mutation default | Donors branch across oral, discussion, upload, preset and other provider-specific behaviors | Advertise execute/verify only for bounded simple-choice/multiple-choice/short-answer Groups whose type cardinality is exact; fail closed otherwise |

## Live-validation gate

After WebUI and Asterism-Plugin/Yunzai are complete and read-only test accounts
are available, before advancing verification:

1. validate Password login, captcha classification, JWT/annotator acceptance,
   expiry and recovery;
2. compare CourseResource and Unit/Section/Micro/Group trees with the visible
   account;
3. record per-Group flags plus Course/Unit completion and duration before and
   after ordinary study;
4. identify the actual heartbeat/telemetry lifecycle without Capture-dependent
   work in the first batch;
5. verify any future execution with independent fresh completion and duration
   reads;
6. commit only sanitized fixtures and a dated verification record.
