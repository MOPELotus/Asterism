# UAI fixture plan

No real account was contacted during the 2026-08-14 audit refresh. Current fixtures are
explicitly synthetic and prove parser invariants only.

## Initial synthetic fixtures

```text
fixtures/providers/uai/
  auth/password-success.json
  auth/password-rejected.json
  auth/password-captcha.json
  auth/user-info-valid.json
  courses/list-mixed.json
  courses/resource-detail.json
  tasks/tree-mixed.json
  tasks/tree-browser-order.json
  browser/course-residence-batch.json
  browser/pong-event.json
  browser/residence-control-restart.json
  browser/residence-target-command.json
  browser/residence-target-result.json
  progress/course-mixed.json
  progress/unit-mixed.json
  progress/course-unit-summary.json
  progress/course-required-policy.json
  duration/unit-mixed.json
  questions/content-multiple-choice.json
  questions/content-mixed-simple.json
  answers/standard-multiple-choice.json
  answers/standard-mixed-simple.json
  submissions/accepted.json
  submissions/verified.json
  submissions/verified-mixed-simple.json
```

They cover typed Password success/rejection/slider outcomes, strict atomic
openid/JWT parsing, bounded user-info identity validation, Course →
CourseResource flattening, paired point counts, fresh detail binding, redacted
Course-instance routing, the outer/nested Course-tree envelope,
Unit/Section/Micro/Group identity separation, bounded task type/question count,
numeric and numeric-string Course publish versions, an independent
Course-progress Unit map and version cross-check, versioned Task fingerprints,
identity-bound per-Unit flags, required/minimum-score/statistic-mode strategy,
strict bounded availability windows, raw progress duration and bounded
text/video tab type, exact per-Unit account/Course/Unit route echoes and a
publish-version cross-check, independent Course/Unit aggregate finish-progress,
learning-seconds, score and required facts, Course/Unit unlock, scoring, window
and ordered required-Task policy facts, and an exact
CourseResource/Unit/Group-bound Task study record containing bounded completion
percentage, learning seconds, required, score-task and optional total-score
facts. Inline negative tests cover
malformed/extra composite
fields, duplicate
resources/groups, misbound details/contexts, unknown roles and impossible point
totals, plus duplicate study-record nodes, missing/negative/overflow seconds
and malformed completion/required/scoring facts.
The encrypted question/answer fixtures additionally cover bounded `unipus.`
framing, separate decryptions, exact Group/question binding, a mixed ordered
two-module simple Group with bounded per-child `type`/`replyType` judge labels,
content-derived `video-popup` choice/text shapes, rejection of incompatible
child shapes, and removal of key material. Their synthetic native
instance identities are positive numeric, matching the mutation shape
independently observed in the donors. Submission fixtures cover one accepted
version receipt plus single- and multi-module exact receipt-versioned
user-module readbacks. They use only synthetic answer values; tests require
Course/Group/version/question order/submitted-state/answer equality and reject
changed or reordered rows. A missing receipt produces Inconclusive without
making a transport request.

Media parsing fixtures cover audio/video content paths, subtitle tracks,
bounded embedded WEBVTT cue extraction with header/ordinal/timestamp removal,
stable Task+remote-Question+URL-bound opaque attachment IDs,
fragment-free HTTPS routes, URL redaction, different IDs for the same URL
under another Task, and rejection of non-HTTPS or literal-IP routes. Durable
artifact fixtures cover deterministic encoding, redacted debug output, exact
Task/Group/every-Question/position/content-fingerprint manifest binding,
ordered attachment-set equality, URL/kind/attachment-ID recomputation, unknown
fields, digest substitution, reordered snapshots and the encoded-size ceiling.
Parser-adapter coverage atomically consumes the complete cached reference set,
aggregates media entries into one Core continuation, rejects partial/replayed
attempts and returns no artifact for an entirely media-free set. Session-aware
AnswerResolve accepts the exact type/phase/revision/digest-bound artifact,
rebinds it before fresh content/answer reads, keeps the legacy `None` path and
rejects foreign metadata without transport. Persisted Domain Questions never
contain a fetch URL; only Core's encrypted QuestionSession continuation can
retain the Provider-private route. Fetch-plan fixtures select one exact bound
attachment, retain its Task/Question/fingerprint position, produce a stable
nonzero request digest, enforce separate subtitle/audio/video byte ceilings,
keep external hosts and custom ports anonymous, permit the session only for the
standard `https://ucontent.unipus.cn` origin, redact route/binding Debug output and reject foreign
Question/attachment identities plus every redirected final route. Response
fixtures move synthetic bytes into a zeroizing owner, require exact status 200,
the original final route and a nonempty body within the frozen ceiling, bind
the byte digest and length to the request digest, redact body/route/binding and
reject partial status, any nonzero reported redirect count, empty, oversized or
route-substituted responses.
Downloaded-subtitle fixtures reuse the evidenced VTT/SRT line normalizer,
remove headers, cue ordinals, timestamps and rich-text markup, retain the exact
response owner and bind the normalized secret-text digest to its response
digest. Non-subtitle media, invalid UTF-8, empty cue sets and oversized sources
fail before a transcript owner exists; audio/video transcription remains
outside this Provider fixture boundary.
Ordered fetch-batch fixtures retain the complete audio-then-subtitle source
sequence from one rebound Question, reproduce the same nonzero digest for the
same manifest, change the digest on reversal and reject a duplicate request or
foreign Question. The batch contains no hidden retry, skip, transcription or
prompt policy. Complete-response-set fixtures require one accepted response per
ordered request, reproduce their set digest, change it with one response body
and reject missing, duplicate or reordered slots. They do not represent a
partial-success decision.
DNS/private-range resolution and bounded streaming execution remain the shared
downloader boundary.

## Required live-sanitized fixtures

```text
fixtures/providers/uai/
  auth/password-success-live.json
  auth/password-captcha-live.json
  auth/password-rejected-live.json
  courses/list-empty.json
  courses/list-mixed-live.json
  courses/resource-detail-live.json
  tasks/tree-unit-section-micro.json
  progress/group-pending-in-progress-completed.json
  progress/total-and-unit-duration.json
  duration/unit-task-situation.json
  questions/content-simple-types-live.json
  questions/answer-simple-types-live.json
  submissions/accepted-live.json
  submissions/verified-live.json
  submissions/preset-completion-live.json
```

Remove usernames, passwords, JWT/openid values, annotator tokens, user IDs,
class/school IDs, instance routes, real names, answers, scores, free-form
content, media URLs and request identifiers. Preserve only bounded structural
field names, placeholder identities and response/result codes.

## Regression requirements

- Numeric and string CourseResource IDs normalize identically.
- A decodable standard JWT `exp` bounds persisted/session-cache lifetime but never replaces native user-info validation.
- Provider metadata and Authentication expose the same validated Capture recipe v4: `AssistedSession` reads same-snapshot `u-openid`, raw `Authorization`, optional current-donor `u-school` and the exact `ucontent` Cookie header into required `ProviderCompositeSession` JSON plus purpose-bound `ProviderCookie` outputs. Ordered composite alternatives prove school is preserved when present but is not required for accounts that omit it. Stored-session fixtures prove Cookie/school are account/reference bound, zeroized, injected with donor client headers only on `ucontent`, and never forwarded to `uai`/`ucloud`; ManualImport, CaptureTool and BrowserExtension remain validated non-renewable `Jwt` imports.
- Runtime schema v3 validates conservative Provider/account concurrency and account scan defaults plus bounded Provider/account/Task overrides for residence seconds, video playback, pure-study `marker|placeholder` and direct-discussion `marker|placeholder` modes; mutation paths reject incomplete or stale-schema snapshots.
- Expired native Composite metadata may enter only the exact atomic Password renewal path; expired imported JWT cannot renew.
- Duplicate resources or Groups fail the complete scan.
- `courseInstanceId` remains operation-only and absent from serialized Tasks.
- Stable Group identity contains CourseResource, Unit and Group components.
- Fresh native Task inventory accepts only a bounded positive numeric or numeric-string Course `publish_version` from the independent Course-progress route, requires its complete Unit-key set to equal the fresh tree Unit set, cross-checks any tree version, fingerprints the result, carries it into fresh TaskDetail and emits the current donor judge pair `{course: publish_version, answer: 3}`. A tree without a version is filled from Course progress. Only an explicitly injected legacy fixture transport that omits Course progress can select the separately evidenced MIT `{course: 0, answer: 0}` compatibility pair; zero, negative, oversized, conflicting and structurally different values fail before mutation.
- Course-progress strategy fixtures require one typed strategy object per Unit, retain required/minimum-score/statistic/window facts separately from per-Group strategy and reject invalid booleans, percentages or epoch windows. Empty Unit maps remain valid for a newly empty Course, while oversized maps fail closed.
- Fresh TaskDetail must rediscover that exact identity and reject disappearance or drift.
- Group question counts are optional bounded facts and never authorize a submission by themselves.
- Course point totals never imply Group completion.
- `base` labels never imply executable or assessment capability.
- Unknown tree roles fail closed.
- Progress duration remains untyped and independent from completion.
- Native per-Unit progress must echo the exact session `open_id`, fresh `tutorialId` Course instance and requested `unit_id`; foreign or malformed values fail before a document reaches inventory/progress/execution consumers. Its optional positive publish version must agree with the Course/tree snapshot, while zero, negative, oversized or conflicting values fail closed.
- Study-record parsing requires one exact unique CourseResource/Unit/Group binding and freezes those route components plus the complete normalized remote Task ID in the returned owner. It preserves bounded Task `finishProgress`, duration seconds, optional `required`, `scoreTaskFlag` and `taskQuesTotalScore`; DurationRead returns only the seconds projection. Missing optional facts remain absent, while malformed route components, percentages, booleans or total scores fail closed.
- Aggregate progress requires an exact CourseResource and fresh app-user binding, one bounded Course total and unique `role=unit` rows; out-of-range/negative metrics, duplicate Units, foreign users and invalid required/role shapes fail closed while personal and path noise is discarded.
- Required policy refreshes the positive strategy ID from Course detail, POSTs the exact CourseResource/strategy pair and requires both identities throughout the response. Fixtures preserve donor required-Task order and reject duplicate Unit/task/sort identities, foreign strategy/resource, unsupported node type, non-binary unlock mode and invalid score/time fields.
- Content and standard answers remain independent fresh documents; neither may be reused as submission evidence.
- Nested content encoded as JSON text or supplied as an inline object normalizes identically under one size/zeroization boundary; rich `text`/`pcText` and per-component stems are preserved without HTML markup.
- Short-answer standard-answer fallback accepts bounded plain analysis text, a top-level JSON analysis string and ordered child analysis rows, but never arbitrary JSON shapes.
- Decrypted byte owners zeroize on parse success and failure; raw parsed JSON owners redact Debug output and recursively zeroize nested string values after normalization.
- Executable answer-bearing and preset request JSON uses a zeroizing owner on every local success, error and cancellation path.
- Question snapshots remain bound to the exact stable CourseResource/Unit/Group Task and cannot build a draft for another route.
- Submission previews contain no selected answer values or executable provider payload.
- Bounded positive choice, multi-child composite choice, short-answer/writing, translation/revise, fillblank/banked-cloze, ordering, matching and content-derived `video-popup` Groups with one shared type or exact one-type-per-Question cardinality advertise execute/verify. MIT-donor `writing` analysis is tested through Question→Answer→Draft→Submission as bound text; `video-popup` tests derive choice or ordered text children from fresh `replyType`, reject incompatible mixtures and retain a non-empty answer body with `submitType=2`; composite option sets, matching-left and ordering child facts remain explicit.
- Multi-Question execution preserves draft positions, native module identities, per-module children and flattened judge/completion order; fresh count/type drift fails before mutation.
- Every Question execution requires one bounded content-derived current-donor judge descriptor per answer child and never substitutes the Group `base` when that exact metadata is absent.
- Submission execution requires a positive numeric native instance ID and rejects arbitrary read-only Question identities before transport. Its native path refreshes exact Unit progress and rejects a completed, non-task or out-of-window Group before building or sending the POST. The final zeroizing request digest remains stable for an identical method/URL/content-type/body and changes with Course instance, openid or any selected-answer body byte; Debug output exposes neither route, account nor answer material. Artifact-bearing fixtures require the exact complete media type/phase/nonzero-revision/digest before transport preparation, expose a nonzero `uai.answer-submit.v1` request digest while the mutation count remains zero, then execute once into a terminal receipt with a distinct response digest. Foreign session metadata and foreign provider/account execution contexts fail before transport. Ambiguity fixtures freshly rematerialize and compare the exact operation type/revision/request digest without increasing the mutation count; changed material fails closed, while an exact match returns no recovery outcome and remains locked because no receipt version can be proved. Entirely media-free snapshots continue through the legacy path.
- Codes `600001` and `600002` never produce a receipt and never trigger an implicit mutation retry.
- Ambiguous mutation transport failures return after exactly one Provider attempt.
- Completion execution uses the five preset labels, exact `exit-ticket`, exact single discussion, exact `short_answer`, or one exact single oral family only as candidates. It requires a fresh exact Unit/Group `tab_type=text|video` leaf for presets or `tab_type=task` for exit-ticket/discussion/subjective/oral, enforces the donor's strict availability window and skips already-completed leaves. Preset fixtures freeze either the exact no-Question `submitType=2` marker or Apache's `submitType=1` placeholder: comma-split base types truncate to `question_num` or repeat the final type, every `instanceId=0` Question retains the bounded score map, and each ordered judge uses the fresh positive Course publish version. Discussion independently covers marker and homogeneous placeholder settings. Exact `short_answer` advertises ResourceExecution beside ordinary SubmissionExecute/SubmissionVerify and its separately authorized empty path emits one bounded `instanceId=0` placeholder/judge per declared Question using the fresh Course version. Oral retains its bounded placeholder. Every ResourceExecution mode materializes its final common POST through the redacted zeroizing request owner: identical complete marker wires have the same nonzero digest, a placeholder body differs, and Debug exposes no route/account/task material. Each mode mutates at most once and exposes a goal-bound fresh-detail plus exact-progress verify-only path for Core recovery; missing/zero publish versions fail before placeholder transport, receipts/synthetic bodies are never completion evidence, and shared TaskExecution still needs to register the digest before dispatch.
- Verification requires the receipt version and an exact fresh Course/Group/complete ordered Question set/submitted-answer readback. Artifact-bearing verification rebinds the exact complete media session manifest before either fresh Task or user-module read; foreign type/phase/revision/digest material leaves the verification transport untouched, while the no-session verifier remains available for media-free snapshots.
- Missing receipts are Inconclusive; receipts alone never confirm success, score, progress or completion.
- Completion-diagnosis fixtures require the exact `uai.empty-completion-verification.v1` schema, fresh-progress marker, unmatched goal and `window_closed=true` before returning `WindowClosed`. An unbounded/future window, false or missing fact, foreign schema, completed outcome, or ordinary SubmissionVerify snapshot returns no Provider diagnosis. Individual `pass/pass2/perm` zeros, total score, duration and required flags are never converted into guessed shared reasons.
- Receipt-versioned user-module score fixtures require a bounded `__SUMMARY__.answerList`; only numeric donor-counted `questionType=1|3` authorizes the same readback's `__SUBMIT_INFO__.state.score_avg`. Decimal `0..=100` converts to fixed-point thousandths, zero remains present, uncounted or absent summaries return no score, and malformed/oversized summaries or out-of-range values fail closed. Ordinary, upload and compound verification paths share this parser.
- The scored ordinary-verification, exact single-upload, ordered choice/upload and ordered matching/oral readbacks cover the same complete receipt-bound user-module policy. Their Provider-private owners preserve the exact result digest, Group/version, positive strategy identity, required/recording booleans, `task_mini_score_pct=60.000%`, second-resolution availability window and submit state/last-submit time while redacting bindings from Debug. Legacy documents with both strategy fields absent still verify without policy evidence; partial or invalid policy fails closed. Each adapter must first prove its immutable answer, object key or oral value/extra and exact module order before returning the owner. This Provider-local preservation does not remove the shared compound Draft/Attempt/Artifact contract gap.
- Future Answer Evidence Corpus fixtures must keep exact submitted `quesData` values, optional score and `answerList.done/right/student_answer/versions` distinct. A module-level Composite cannot consume child summary rows by JSON-map order; a fixture becomes eligible only after the shared child-evidence/provenance contract binds every row explicitly.
- First-bind bootstrap has no UAI answer-history fixture or executable route. Tests must assert that a missing receipt/version performs no user-module I/O and that read-only Course/Task/progress discovery cannot be reported as a complete answer harvest. The unused Rust wall-clock-suffix helper is not a fixture authority.
- Strict Completion fixtures continue to require fresh `pass=pass2=perm=1`. Score Improvement fixtures must expose UAI retake as unavailable/unknown after completion: all donors skip completed Groups, Rust issue 4 is only a feature request, and `record_every_submit`/`record_max_submit` do not authorize a redo. Group, user-module, Unit and Course score thresholds remain independently typed until shared policy reconciliation exists.
- Every Group advertises BrowserBridge, but a session spec is issued only after fresh exact Task rediscovery; v2 carries the exact secret-free `https://ucontent.unipus.cn/_explorationpc_default/pc.html?cid={fresh CourseResource}` start route, the isolation key hashes account plus Task, the origin set is exactly `ucontent`/`ipub`, and `read_sources` is explicitly empty. Shared spec validation proves the start origin belongs to that allowlist, and the URL contains no token/openid material.
- Provider-private BrowserBridge plan fixtures cover the exact secret-free HTTPS `/_explorationpc_default/pc.html?cid={fresh CourseResource}` start route, Course/Group/normalized-route consistency, rejection of extra queries/userinfo/fragments and fresh normalized Unit/Section/Micro/Task rebinding. Plan v2 now freezes the donor's four ordered iframe selectors, 30-second scan observer, 1.5-second retry interval and 20-retry ceiling alongside the four legacy `pc-slider`/Ant Tree/ARIA menu/`u3menu` discovery families, exact current Tab/Task/popup/video selector allowlists, frozen residence/video settings, 2048 discovered-Micro/64-Tab/128-Task and timing ceilings, and mandatory session-nonce/frame/exact-origin message security. Changing, reordering, adding or dropping any selector, or changing any iframe scan bound, fails validation. Typed message fixtures cover correlated `SCAN`→bounded menu list, unique exact hierarchy selection, opaque session-bound handle→`CLICK` result, bounded `PING`→`PONG`, and pause/resume/restart controls that retain the immutable Task handle and residence budget while bounding the restart Micro ordinal. They reject a missing or duplicate target, transport/envelope origin mismatch, foreign nonce/frame/Task, reordered menu ordinals, command/event mismatch, unbounded labels, arbitrary CSS/DOM paths and unexpected JSON fields. Source audit distinguishes the iframe-only donor command channel from direct top-page Tab/Task actions; Asterism binds both through typed commands. Top-page fixtures enumerate ordered Tab and Task snapshots, permit only validated Tab handles, and require a unique exact fresh Task-title match before constructing a Task click or the single final residence action. Result fixtures bind target handle and command sequence, bound counts/time and never replace fresh DurationRead. Rendered execution fixtures must additionally cover Micro→Tab→Task ordering, pause/restart accounting, video state and actual DOM dispatch.
- The residence-target command fixture also freezes the exact typed wire fields before helper dispatch. Its encoded command artifact is bounded, redacted and digest-identical to the ordinary exchange identity; recovery accepts only the same fresh plan, session nonce, origin, frame, Task and sequence. Digest changes, foreign bindings and oversized artifacts fail before any result can be parsed.
- Helper projection fixtures consume Core `SecretValue` command bytes and distinguish exactly `ScanMenu`, `ClickMenu`, scoped `ScanPage`, `ClickTab`, `ClickTask`, `Residence`, `ResidenceControl` and `Ping`. Their safe profile repeats every compile-time discovery/iframe/Tab/Task/popup/video selector and cardinality/timing/security bound without accepting command-provided selectors. Digest, session ID-derived nonce, actual origin/frame, u64-to-u32 sequence, wire revision, Group ID and opaque action handles are checked before projection; Debug redacts frame/Task/handles and the secret owner is dropped. Residence control preserves only the exact Task handle, leaf budget and bounded pause/resume/restart value; its typed result encoder accepts only a matching acknowledgement at or below that budget. The durable acknowledgement fixture issues the control through its command artifact/exchange, completes it in memory, then resolves the same raw-result inbox and recovers only the persisted control; a changed control fails while the redacted owner retains the observed active seconds and completed result digest. Foreign bindings, oversized bytes, unsafe handles and unknown `selector`/`script` keys fail without performing DOM work. Core still owns cancellation and sequencing of an in-flight Residence exchange.
- Closed helper-DOM/runtime recipe fixtures compile from those validated projections only. Menu asserts the exact ordered 12 container selectors, four legacy/Ant/ARIA/u3menu descriptors, separate row/hierarchy-text/leaf/clickable selector fields, text-property fallback order, four iframe selectors and scan bounds. Both Tab and Task scopes assert their exact selector-specific text fields, active rules and 64/128 ceilings. All three click actions retain the identical projected opaque handle and exact centered-scroll plus four-mouse-event order. Residence retains its identical Task handle, requested budget/video flag, five popup and two video selectors, 3-second DOM poll, 1-second video poll, 16-click and 1,800-second ceilings; Residence control is selector-free and freezes only the same handle/budget plus closed control enum; Ping remains fully action-free. Debug exposes no handle, selector or budget, and internal changes to cardinality, event order or poll bounds fail with protocol drift. There is no raw JSON/script/arbitrary-selector constructor and compilation performs no DOM operation.
- Typed helper-result fixtures feed action-matched Menu/Page label rows, click acknowledgements, residence facts and Pong into the validated projection. The projection reconstructs nonce/origin/frame/Task/sequence and opaque handles, emits exact `uai.browser.event` or `uai.browser.residence.result` plus zeroizing bytes/digest, and every document passes the existing plan/command parser unchanged. Projection Debug redacts session/origin/frame/Task/sequence, result Debug redacts bytes, and callers never provide raw JSON or echo bindings. Mismatched observations, reordered/oversized rows, unsafe labels, Tab overflow, active-time overflow and video facts under a video-disabled action fail closed. A 2048-row fixture with three maximum 512-byte labels per row exceeds 1 MiB but remains within the exact 8 MiB Provider event bound and parses successfully, preserving the donor menu ceiling.
- Raw intermediate event and terminal residence-result owners are independently bounded, zeroizing and redacted. They consume Core result-inbox `SecretValue` directly, validate size and UTF-8 over the borrowed secret bytes, and hash/parse that same buffer without an ordinary plaintext `String` copy; invalid UTF-8 fails at construction and drops the zeroizing owner. The consuming inbox wrapper also requires an issued UAI exchange and exact Core result type/session/sequence/digest with `received_at >= issued_at`; a residence digest substitution or event/residence type swap fails before fresh Provider reads, while successful completion uses only that trusted receive time. Their digests equal the exact raw helper documents used by the durable ledger, their Debug output exposes neither nonce nor page labels, and only their typed parser methods can produce validated event/result envelopes.
- The disposition-classified cursor-result fixture proves the registry-visible credential set is empty, the Intermediate set is exactly `[uai.browser.event]` and the ExecutionTerminal set is exactly `[uai.browser.residence.result]`. It drives classifier and strict inbox assertions from those sets; command/empty/near-match/case-changed/foreign-Provider types remain undeclared, classify as `None` and cannot construct an inbox. A foreign Core session and changed result digest also fail before fresh Provider reads. End-to-end persistence fixtures consume the issued command plus independent `uai.browser.cursor.v4` sidecar, fresh-rebind the complete Course batch, Task/settings plan, command and recovered cursor stage, then parse the exact observed-origin result. A sequence-1 MenuList can produce only a completed event exchange plus immutable sequence-2 `ClickMenu`/`ClickingMenu` advance. A sequence-7 full residence result can produce only a completed terminal exchange plus a non-resumable checkpoint retaining its exact digest/time/remaining budget and fresh-DurationRead requirement. Both owner Debug views redact their state.
- Shared workflow-callback fixtures move the compact child projection into Core's encrypted workflow-plan owner, then require Provider-owned fresh Course/ordered-Task inventory to reproduce its embedded profile, Task fingerprints, start and batch digests under the frozen settings. Intermediate returns the shared constructor's exact completed exchange plus contiguous sequence-next command and cursor sidecar. ExecutionTerminal performs independent fresh DurationRead and returns `RemoteState::Unknown`, no percent, exact duration and a post-result observation time through the shared terminal constructor. Missing workflow plan/cursor, changed command/result/state bytes, a foreign runtime frame and changed frozen settings all fail closed. Production Provider composition injects the same CourseInventory, TaskInventory and Duration boundaries into BrowserBridge; Engine/daemon callback invocation remains the shared gap.
- Issuance fixtures bind the dispatched typed command, its encrypted artifact and the Core exchange to one session/sequence/digest. Recovery uses only that persisted exchange plus the decrypted artifact to restore the command, then freshly rebinds the Task and immutable settings before parsing. The `pong` fixture completes both the in-memory and recovered paths; pairing the same helper event with a separately issued `SCAN` command fails even though both commands are otherwise valid. The terminal residence fixture binds sequence and target handle, completes with the exact raw result digest, and remains an observation requiring fresh DurationRead.
- Course residence fixtures deliberately put `unit-z`/`group-z` before lexically smaller identities and keep two ordered Groups under one Micro. Task inventory must retain that fresh source-tree order while duplicate detection remains independent. The immutable batch plan freezes three ordered Micros, Course publish version, complete Group shape, a start target at ordinal 1, total 1,200-second/video settings and the exact remaining two-Micro rational denominator. A 3-Tab/7-Task leaf rounds to 29 seconds only at runtime. Restart to ordinal 2 must carry that Micro's identity digest, retain full membership/total budget, change the plan digest and recompute the denominator to one. Progress-only changes rebind; reordered membership, changed labels, non-contiguous Micros, foreign restart digests and DOM cardinality overflow fail closed.
- Credential-free child-plan fixtures freeze child/batch versions, Course identity/publish version, an explicit nonzero runtime-profile digest, the owner Task under start ordinal 1, every ordered Task ID/fingerprint and the exact batch/start digests. Strict 8 MiB bounded JSON round-trips and freshly reconstructs the cursor-start batch; Debug contains no Course, owner or fingerprint material. Its compact namespaced Core artifact retains exact identity/start/batch facts plus ordered-task/full-child digests. Foreign Provider/type, unknown artifact fields and changed fresh fingerprints fail before recovery. A synthetic 8192-Task maximum with maximum platform-shaped Task IDs proves the full child exceeds 64 KiB while the accepted Core artifact remains below 64 KiB, so no donor task is truncated. A foreign start owner, zero/changed profile digest, changed settings, changed fingerprint and reordered inventory all fail closed before cursor construction.
- Accumulated residence cursor fixtures encode a separate version-4 zeroizing artifact whose SHA-256 digest is stable for identical state and whose Debug output exposes no page labels or handles. Recovery accepts only the exact fresh Course batch plan, serialized Browser residence plan and next command/session; a restart, changed runtime plan, different valid command, digest mismatch, unknown field, empty document or document above 256 KiB fails closed before browser dispatch.
- Cursor-aware issuance fixtures return the dispatch command, independent command/cursor artifacts and matching durable exchange as one owner. Its consuming Core handoff freezes state type `uai.browser.cursor.v4`, cursor digest and exact session/sequence/issued-at metadata while moving both secret artifacts without plaintext copies. Strict consuming recovery requires the exact Core command plus sidecar metadata/artifact, rejects a downgraded/foreign state type before fresh reads, then selects the command through the persisted exchange and verifies the separately persisted cursor digest plus exact fresh Course/Browser hierarchy. Substituting another otherwise-valid command issued under the same session and sequence fails closed; sidecar bytes never enter helper dispatch.
- The first accumulated transition fixture completes the initial `ScanMenu` through the ordinary validated raw-event owner, selects one exact fresh Unit/Section/Micro menu entry, and returns an immutable `ClickMenu` cursor/command at sequence 2. The next cursor retains the exact prior raw-result digest and next-command digest while the original cursor remains unchanged; replacing the completed exchange's command digest fails closed.
- The next transition accepts only the exact completed opaque menu click, replaces prior-result authority with that click's raw digest and returns an immutable `ScanningTabs` cursor with sequence-3 `ScanPage(Tab)`; the `ClickingMenu` cursor remains unchanged.
- Tab-scan transition fixtures freeze the complete validated ordered Tab snapshot. A non-empty two-Tab result authorizes only ordinal-zero `ClickTab` and records current/next ordinals, while an empty Tab result moves directly to `ScanPage(Task)` with no fabricated handle.
- An accepted current-Tab click retains the frozen Tab snapshot and ordinal cursor, leaves processed-Tab count unchanged until that Tab's Tasks finish, replaces prior-result authority with the click digest and advances to sequence-next `ScanPage(Task)`.
- Task-scan transition fixtures freeze the complete ordered current-page snapshot. One exact fresh title authorizes only its opaque `ClickTask`; a page without that title clears the stale Task snapshot, increments the inspected-Tab count and selects the next frozen Tab handle. Missing the target after the final Tab fails `RemoteChanged` without changing the immutable cursor.
- An accepted exact Task click derives its `ResidenceTarget` seconds from the Course batch rational share and frozen runtime cardinalities, never by copying the total plan budget. The three-Micro/two-Tab/two-Task fixture yields 100 rather than 1,200 seconds; the direct two-Task/no-Tab path yields 200 seconds and remains valid without a fabricated processed Tab. Ordinary command issuance rejects the leaf budget, while cursor-aware issuance accepts the exact bound command/artifact pair.
- Terminal residence fixtures pass the exact 100-second leaf through cursor-aware issuance, raw result ownership and completed exchange correlation before producing a non-replayable Provider checkpoint with 1,100 seconds remaining and exact Micro/Tab/Task counts. A 99-second or cancelled observation cannot produce the checkpoint. It has no next-command authority and always requires fresh DurationRead. The independent duration transport then reads the exact checkpoint Task and produces a verify-only readback only when Course/Unit/Group, Course-batch digest, Browser-plan digest, current Micro and `observed_at >= completed_at` all agree. The owner preserves 700 observed seconds plus the richer study-record facts without asserting a required delta; a foreign Browser plan fails before duration transport.
- Browser residence execution fixtures remain required as its shared executor lands; their absence is a tracked implementation gap, not a first-batch deferral. Exit-ticket, exact single-oral-family, single-upload, mixed choice/upload and ordered matching/oral Provider mutation boundaries are covered offline, pending shared registration and live validation. Mixed oral fixtures bind the one matching Draft to a fresh exact `basic-scoop-content,oral-sentence` Task and two-entry encrypted answer array, preserve the oral instance plus scalar-empty, empty-array or donor-stable non-empty child values and per-child dynamic `answersExtra`, inject the fresh Course publish version, and require one ordered receipt-versioned readback with exact value/extra equality. Its final request digest is stable for identical method/URL/content-type/body and changes with Course instance, account or matching answer; Debug output redacts route and body. Reordered modules, changed instances, oversized material, non-text list members and object values without stable donor judge encoding fail before mutation.
- Upload boundary coverage starts at the Task-tree parser: exact and case-equivalent platform spellings normalize to the donor-canonical `multiFileUpload`, never the unreachable `multifileupload`, for both single and ordered mixed Groups. It then requires fresh exact Group hierarchy rebinding, one unique positional upload module, a Task-fingerprint/artifact-digest-bound intent, code-200 grant parsing, redacted/zeroizing token ownership, exact file-key retention, grant/artifact equality before multipart emission, the donor's 4 KiB minimal MP3, bounded audio/mpeg artifacts, deterministic multipart framing and exact key equality in object-store readback. The fresh CMS request digest changes with Course/app-user query identity, the Qiniu digest changes with its token/body, and each single/compound final-submit digest changes with any route/account/body material; identical complete requests remain stable and all request Debug output is redacted. The returned uploaded-artifact owner retains Task fingerprint, Course/Unit/Group/module/object-key/digest/intent binding and redacts the route. Single-upload final-plan coverage re-reads the same Task, requires position 1 and a fresh positive publish version, and emits the donor `instanceId=0` key child inside the minimal current envelope. The mixed path joins one validated ordinary sub-draft and upload position 2 only after fresh exact `multichoice,multiFileUpload` rediscovery; its body and readback must retain the choice module followed by module 0/key in one atomic attempt. Reversed, extra or changed modules, wrong upload position and split execution fail closed. Native mutation separately rebinds Course instance and an exact incomplete available task progress leaf. Even confirmed answer persistence still requires fresh progress.
- Discussion synthetic coverage first proves both independently authorized direct-empty modes: only one exact single `discussion` Task advertises ResourceExecution, native preflight requires a fresh open `tab_type=task` leaf, and the frozen runtime snapshot selects either the default empty `submitType=2` marker or the donor `submitType=1` `instanceId=0` placeholder with empty child, score map, discussion judge and fresh Course publish version. Both mutate once and only fresh exact progress verifies them. The richer flow separately binds fresh Course/class/curricula/current-user facts, a unique first-page topic, bounded 20-row reply pages to the exact requested topic, a redacted/zeroizing immutable reply intent, accepted-but-unverified reply receipts and exact current-user/content readback. Reply request digests change with route/account/topic/content; completion request digests separately bind the fresh Task fingerprint/hierarchy/topic and verified reply digest. Only the exact topic/user/content page plus a fresh exact single-discussion Task authorizes the second plan; native completion performs the same preflight and empty Group POST, whose receipt still requires fresh progress verification. Foreign topic/content, compound Task and identity drift fail before the second mutation. Durable cross-mutation Draft/attempt registration remains shared integration work.
- Protocol-observation tests inject credential-shaped decoys beside an unknown top-level Task-tree role, unknown string/object `video-popup` reply types and an invalid score-summary `answerList`. They require exact surface/kind/schema plus only approved digests, byte lengths, JSON kinds, presence and cardinalities, and prove the encoded observation contains neither the original unknown values nor decoy answer/authorization material.
