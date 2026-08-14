# UAI fixture plan

No real account was contacted during the 2026-08-13 audit refresh. Current fixtures are
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
  browser/residence-control-restart.json
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
Task/Group/Question/position/content-fingerprint binding, ordered attachment
set equality, URL/kind/attachment-ID recomputation, unknown fields, digest
substitution and the encoded-size ceiling. Parser-adapter coverage additionally
requires the Question and artifact to derive from the same ephemeral entry,
redacts the combined result and returns no artifact for media-free Questions.
Persisted Domain Questions never
contain a fetch URL; only Core's encrypted QuestionSession continuation can
retain the Provider-private route. DNS/private-range resolution,
redirect handling and host-scoped authenticated media fetching remain the
shared downloader boundary.

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
- Study-record parsing requires one exact unique CourseResource/Unit/Group binding and preserves bounded Task `finishProgress`, duration seconds, optional `required`, `scoreTaskFlag` and `taskQuesTotalScore`; DurationRead returns only the seconds projection. Missing optional facts remain absent, while malformed percentages, booleans or total scores fail closed.
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
- Submission execution requires a positive numeric native instance ID and rejects arbitrary read-only Question identities before transport. Its native path refreshes exact Unit progress and rejects a completed, non-task or out-of-window Group before building or sending the POST.
- Codes `600001` and `600002` never produce a receipt and never trigger an implicit mutation retry.
- Ambiguous mutation transport failures return after exactly one Provider attempt.
- Completion execution uses the five preset labels, exact `exit-ticket`, exact single discussion, or one exact single oral family only as candidates. It requires a fresh exact Unit/Group `tab_type=text|video` leaf for presets or `tab_type=task` for exit-ticket/discussion/oral, enforces the donor's strict availability window and skips already-completed leaves. Preset fixtures freeze either the exact no-Question `submitType=2` marker or Apache's `submitType=1` placeholder: comma-split base types truncate to `question_num` or repeat the final type, every `instanceId=0` Question retains the bounded score map, and each ordered judge uses the fresh positive Course publish version. Discussion independently covers marker and homogeneous placeholder settings. Oral retains its bounded placeholder. Every mode mutates at most once and exposes a goal-bound fresh-detail plus exact-progress verify-only path for Core recovery; missing/zero publish versions fail before placeholder transport, and receipts/synthetic bodies are never completion evidence.
- Verification requires the receipt version and an exact fresh Course/Group/complete ordered Question set/submitted-answer readback.
- Missing receipts are Inconclusive; receipts alone never confirm success, score, progress or completion.
- Receipt-versioned user-module score fixtures require a bounded `__SUMMARY__.answerList`; only numeric donor-counted `questionType=1|3` authorizes the same readback's `__SUBMIT_INFO__.state.score_avg`. Decimal `0..=100` converts to fixed-point thousandths, zero remains present, uncounted or absent summaries return no score, and malformed/oversized summaries or out-of-range values fail closed. Ordinary, upload and compound verification paths share this parser.
- Every Group advertises BrowserBridge, but a session spec is issued only after fresh exact Task rediscovery; the isolation key hashes account plus Task and the origin set is exactly `ucontent`/`ipub`.
- Provider-private BrowserBridge plan fixtures cover the exact secret-free HTTPS `/_explorationpc_default/pc.html?cid={fresh CourseResource}` start route, Course/Group/normalized-route consistency, rejection of extra queries/userinfo/fragments and fresh normalized Unit/Section/Micro/Task rebinding; they also cover the four legacy `pc-slider`/Ant Tree/ARIA menu/`u3menu` discovery families, exact current Tab/Task/popup/video selector allowlists, frozen residence/video settings, 2048 discovered-Micro/64-Tab/128-Task and timing ceilings, and mandatory session-nonce/frame/exact-origin message security. Typed message fixtures cover correlated `SCAN`→bounded menu list, unique exact hierarchy selection, opaque session-bound handle→`CLICK` result, bounded `PING`→`PONG`, and pause/resume/restart controls that retain the immutable Task handle and residence budget while bounding the restart Micro ordinal. They reject a missing or duplicate target, transport/envelope origin mismatch, foreign nonce/frame/Task, reordered menu ordinals, command/event mismatch, unbounded labels, arbitrary CSS/DOM paths and unexpected JSON fields. Top-page fixtures additionally enumerate bounded ordered Tab and Task snapshots, permit only validated Tab handles, and require a unique exact fresh Task-title match before constructing a Task click or the single final residence action. Result fixtures bind the target handle and residence command sequence, allow at most one processed Micro and one bound Task per processed Tab, reject other count/time overflow or video activity when disabled, and never replace fresh DurationRead. Rendered execution fixtures must additionally cover Micro→Tab→Task ordering, pause/restart accounting, video state and actual DOM dispatch.
- Browser residence execution fixtures remain required as its shared executor lands; their absence is a tracked implementation gap, not a first-batch deferral. Exit-ticket, exact single-oral-family, single-upload, mixed choice/upload and ordered matching/oral Provider mutation boundaries are covered offline, pending shared registration and live validation. Mixed oral fixtures bind the one matching Draft to a fresh exact `basic-scoop-content,oral-sentence` Task and two-entry encrypted answer array, preserve the oral instance plus scalar-empty, empty-array or donor-stable non-empty child values and per-child dynamic `answersExtra`, inject the fresh Course publish version, and require one ordered receipt-versioned readback with exact value/extra equality. Reordered modules, changed instances, oversized material, non-text list members and object values without stable donor judge encoding fail before mutation.
- Upload boundary coverage starts at the Task-tree parser: exact and case-equivalent platform spellings normalize to the donor-canonical `multiFileUpload`, never the unreachable `multifileupload`, for both single and ordered mixed Groups. It then requires fresh exact Group hierarchy rebinding, one unique positional upload module, a Task-fingerprint/artifact-digest-bound intent, code-200 grant parsing, redacted/zeroizing token ownership, exact file-key retention, grant/artifact equality before multipart emission, the donor's 4 KiB minimal MP3, bounded audio/mpeg artifacts, deterministic multipart framing and exact key equality in object-store readback. The fresh CMS request digest changes with Course/app-user query identity, the Qiniu digest changes with its token/body, and the final-submit digest changes with any route/account/body material; identical complete requests remain stable and all request Debug output is redacted. The returned uploaded-artifact owner retains Task fingerprint, Course/Unit/Group/module/object-key/digest/intent binding and redacts the route. Single-upload final-plan coverage re-reads the same Task, requires position 1 and a fresh positive publish version, and emits the donor `instanceId=0` key child inside the minimal current envelope. The mixed path joins one validated ordinary sub-draft and upload position 2 only after fresh exact `multichoice,multiFileUpload` rediscovery; its body and readback must retain the choice module followed by module 0/key in one atomic attempt. Reversed, extra or changed modules, wrong upload position and split execution fail closed. Native mutation separately rebinds Course instance and an exact incomplete available task progress leaf. Even confirmed answer persistence still requires fresh progress.
- Discussion synthetic coverage first proves both independently authorized direct-empty modes: only one exact single `discussion` Task advertises ResourceExecution, native preflight requires a fresh open `tab_type=task` leaf, and the frozen runtime snapshot selects either the default empty `submitType=2` marker or the donor `submitType=1` `instanceId=0` placeholder with empty child, score map, discussion judge and fresh Course publish version. Both mutate once and only fresh exact progress verifies them. The richer flow separately binds fresh Course/class/curricula/current-user facts, a unique first-page topic, bounded 20-row reply pages to the exact requested topic, a redacted/zeroizing immutable reply intent, accepted-but-unverified reply receipts and exact current-user/content readback. Reply request digests change with route/account/topic/content; completion request digests separately bind the fresh Task fingerprint/hierarchy/topic and verified reply digest. Only the exact topic/user/content page plus a fresh exact single-discussion Task authorizes the second plan; native completion performs the same preflight and empty Group POST, whose receipt still requires fresh progress verification. Foreign topic/content, compound Task and identity drift fail before the second mutation. Durable cross-mutation Draft/attempt registration remains shared integration work.
