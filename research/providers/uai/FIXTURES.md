# UAI fixture plan

No real account was contacted during the 2026-08-09 audit. Current fixtures are
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
  progress/unit-mixed.json
  progress/course-unit-summary.json
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
numeric and numeric-string Course publish versions, versioned Task fingerprints,
identity-bound per-Unit flags, required/minimum-score/statistic-mode strategy,
strict bounded availability windows, raw progress duration and bounded
text/video tab type, independent Course/Unit aggregate finish-progress,
learning-seconds, score and required facts, and exact
CourseResource/Unit/Group-bound learning seconds. Inline negative tests cover
malformed/extra composite
fields, duplicate
resources/groups, misbound details/contexts, unknown roles and impossible point
totals, plus duplicate duration nodes and missing/negative/overflow seconds.
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
embedded WEBVTT, stable opaque attachment IDs, fragment-free ephemeral HTTPS
routes, URL redaction and rejection of non-HTTPS or literal-IP routes.
Persisted Questions never contain a fetch URL; DNS resolution and authenticated
media fetching remain the shared downloader boundary.

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
- Runtime schema validates conservative Provider/account concurrency and account scan defaults plus bounded Provider/account/Task overrides for residence seconds and video playback; mutation paths reject incomplete or stale-schema snapshots.
- Expired native Composite metadata may enter only the exact atomic Password renewal path; expired imported JWT cannot renew.
- Duplicate resources or Groups fail the complete scan.
- `courseInstanceId` remains operation-only and absent from serialized Tasks.
- Stable Group identity contains CourseResource, Unit and Group components.
- Fresh Task inventory accepts only a bounded positive numeric or numeric-string Course `publish_version`, fingerprints it, carries it into fresh TaskDetail and emits the current donor judge pair `{course: publish_version, answer: 3}`. An absent field alone selects the separately evidenced MIT `{course: 0, answer: 0}` compatibility pair; zero, negative, oversized and structurally different values fail before mutation.
- Fresh TaskDetail must rediscover that exact identity and reject disappearance or drift.
- Group question counts are optional bounded facts and never authorize a submission by themselves.
- Course point totals never imply Group completion.
- `base` labels never imply executable or assessment capability.
- Unknown tree roles fail closed.
- Progress duration remains untyped and independent from completion.
- Study-record duration seconds require one exact unique CourseResource/Unit/Group binding.
- Aggregate progress requires an exact CourseResource and fresh app-user binding, one bounded Course total and unique `role=unit` rows; out-of-range/negative metrics, duplicate Units, foreign users and invalid required/role shapes fail closed while personal and path noise is discarded.
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
- Completion execution uses the five preset labels, exact `exit-ticket`, or one exact single oral family only as candidates. It requires a fresh exact Unit/Group `tab_type=text|video` leaf for presets or `tab_type=task` for exit-ticket/oral, enforces the donor's strict availability window, skips already-completed leaves, emits either the exact empty `submitType=2` body or oral's bounded `instanceId=0` placeholder body at most once, and exposes a goal-bound fresh-detail plus exact-progress verify-only path for Core recovery; its receipt and synthetic body are never completion evidence.
- Verification requires the receipt version and an exact fresh Course/Group/complete ordered Question set/submitted-answer readback.
- Missing receipts are Inconclusive; receipts alone never confirm success, score, progress or completion.
- Every Group advertises BrowserBridge, but a session spec is issued only after fresh exact Task rediscovery; the isolation key hashes account plus Task and the origin set is exactly `ucontent`/`ipub`.
- Provider-private BrowserBridge plan fixtures cover the exact secret-free HTTPS `/_explorationpc_default/pc.html?cid={fresh CourseResource}` start route, Course/Group/normalized-route consistency, rejection of extra queries/userinfo/fragments and fresh normalized Unit/Section/Micro/Task rebinding; they also cover the four legacy `pc-slider`/Ant Tree/ARIA menu/`u3menu` discovery families, exact current Tab/Task/popup/video selector allowlists, frozen residence/video settings, 2048 discovered-Micro/64-Tab/128-Task and timing ceilings, and mandatory session-nonce/frame/exact-origin message security. Typed message fixtures cover correlated `SCAN`→bounded menu list, unique exact hierarchy selection, opaque session-bound handle→`CLICK` result, and `PING`→`PONG`; they reject a missing or duplicate target, transport/envelope origin mismatch, foreign nonce/frame/Task, reordered menu ordinals, command/event mismatch, unbounded labels, arbitrary CSS/DOM paths and unexpected JSON fields. Top-page fixtures additionally enumerate bounded ordered Tab and Task snapshots, permit only validated Tab handles, and require a unique exact fresh Task-title match before constructing a Task click or the single final residence action. Result fixtures bind the target handle and residence command sequence, allow at most one processed Micro and one bound Task per processed Tab, reject other count/time overflow or video activity when disabled, and never replace fresh DurationRead. Rendered execution fixtures must additionally cover Micro→Tab→Task ordering, pause/restart accounting, video state and actual DOM dispatch.
- Browser residence and final upload-answer fixture families remain required as their shared execution contracts land; their absence is a tracked implementation gap, not a first-batch deferral. Exit-ticket and exact single-oral-family execution boundaries are covered offline, pending live validation; mixed oral/upload compound shapes remain active work.
- Upload boundary coverage requires fresh exact Group hierarchy rebinding, one unique positional `multiFileUpload` module, a Task-fingerprint/artifact-digest-bound intent, code-200 grant parsing, redacted/zeroizing token ownership, exact file-key retention, grant/artifact equality before multipart emission, the donor's 4 KiB minimal MP3, bounded audio/mpeg artifacts, deterministic multipart framing and exact key equality in object-store readback. The returned uploaded-artifact owner retains Task/Course/Group/module/object-key/digest/intent binding and redacts the route. Foreign Task identities, duplicate upload modules, artifact swaps and changed returned keys fail before producing it. Neither the grant nor this object result is treated as a final answer or completion receipt.
- Discussion synthetic coverage now binds fresh Course/class/curricula/current-user facts, a unique first-page topic, bounded 20-row reply pages, a redacted/zeroizing immutable reply intent, accepted-but-unverified mutation receipts and exact current-user/content readback. Durable Draft/attempt registration and final Group completion remain shared integration work.
