# UAI drift register

| Risk | Static evidence | Required response |
|---|---|---|
| Login result/code shape changes | Current donors use string code `0`; captcha uses `1506` | Parse typed bounded envelopes; classify challenges as `HumanRequired` and never log bodies |
| JWT header format changes | Donors pass the returned JWT directly as `Authorization` | Validate through user-info/Course reads; do not guess a Bearer prefix |
| Login/user-info routes redirect or return HTML | Shared client does not follow redirects and both routes are audited JSON APIs | Classify redirects/404 as protocol drift and reject missing/non-JSON Content-Type before parsing |
| JWT/openid are stored or renewed separately | Both values are required by different authenticated routes | Persist one strict ProviderCompositeSession and replace it atomically |
| Imported/native session metadata is confused | Both paths store the same encrypted document with different lifecycle authority | Bind ManualImport to Jwt and NativeProviderLogin to Composite; reject every other pairing |
| Concurrent renewals overwrite a fresher session | Password credentials and composite session form one lifecycle set | Serialize by account, validate the new JWT, then compare-and-replace all three credentials atomically |
| Unverified UAI becomes active accidentally | Native boundaries exist but have not passed live validation | Keep daemon registration disabled by default, require an independent explicit opt-in plus SecretStore and emit a Development warning |
| Course list and detail disagree on instance fields | List has `instanceId`; detail has `courseInstanceId` | Use stable CourseResource ID and refresh/bind detail before tree reads |
| Course tree outer envelope changes | Current response stores JSON text in `course` | Bound both layers and fail closed on non-string, malformed or oversized nested data |
| New tree role appears | Audited roles are Unit, Section, Node, Link and Group | Reject unknown roles and add a sanitized fixture before mapping them |
| Group ID disappears or becomes URL-only | Historical parser falls back to URLs for generic nodes | Require explicit bounded Group IDs; never persist route URLs as Task identity |
| Persisted Group facts become stale before a read | CourseResource detail supplies a fresh instance route and trees can change | TaskDetail re-lists CourseResources and rebuilds the exact Group; missing facts become RemoteChanged |
| Task-detail fingerprints change shape silently | Core requires versioned fingerprints for fresh detail validation | Prefix normalized Group hashes with `v1:` and bump the version when fingerprint material changes |
| Completion flags gain new values | Current donor requires `pass == pass2 == perm == 1` | Keep each flag independent and map only sanitized verified combinations |
| Annotator token claims/signature change | Both backend donors independently use the same HS256 openid claims and millisecond expiry | Keep generation private to account-bound GET reads, pin an exact-time vector and fail closed on header/clock errors |
| Study-record duration unit/meaning drifts | Frozen MIT donor explicitly documents Course/Unit/Task `duration` in seconds | Keep the study-record route independent, bind exact CourseResource/Unit/Group identities and recheck with live before/after measurements |
| Progress duration is confused with study duration | Signed progress response also exposes an untyped numeric `duration` | Preserve progress value as raw only; populate seconds exclusively from the independent study-record response |
| Study-record tree duplicates or changes roles | Donor indexes nodeId and currently emits Unit/Section/Node/Link hierarchy | Require unique bounded node IDs, audited roles and one exact matching Task; reject drift before returning duration |
| Page residence stops producing duration | Current userscript relies on active page lifecycle | Prefer native network evidence; otherwise isolate a headless compatibility worker and verify fresh duration |
| Direct submit completes without duration | Backend donors submit answers independently of page residence | Keep CompletionService and DurationService separate; never claim execution from completion alone |
| Encrypted content/answer framing changes | Donors currently use `unipus.` hex AES-ECB envelopes with a key suffix | Bound and validate every layer before decrypting; never persist ciphertext, key material or route context |
| Submission throttling is mistaken for success | Donor returns `600001`/`600002` for rate limiting | Return typed retry state and never repeat a mutation without Core idempotency/recovery authority |
| Submit receipt is accepted as result verification | Fresh user-module and progress reads are separate donor calls | Keep SubmissionExecute and SubmissionVerify separate and require a fresh identity-bound readback |
| A missing receipt causes verification to guess the current module version | The readback route is keyed by the submit response version | Return Inconclusive without transport I/O when no accepted version receipt exists |
| User-module route and response identify different Courses or Groups | The route uses fresh `courseInstanceId` while state carries separate submit metadata | Bind fresh Course detail, response Course, submit-info Group and both version fields before inspecting answers |
| User-module answer is stale, partial or changed | `quesData` embeds separately encoded context and answer children | Require one exact question, submitted context, every child `isDone`, and exact immutable draft-answer equality |
| Unsupported UAI task kinds acquire a generic mutation default | Donors branch across oral, discussion, upload, preset and other provider-specific behaviors | Advertise execute/verify only for one-question simple-choice/multiple-choice/short-answer Groups; fail closed otherwise |

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
