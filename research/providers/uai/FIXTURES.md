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
versioned Task fingerprints, identity-bound per-Unit flags plus raw progress
duration and bounded text/video tab type, and exact
CourseResource/Unit/Group-bound learning seconds. Inline negative tests cover
malformed/extra composite
fields, duplicate
resources/groups, misbound details/contexts, unknown roles and impossible point
totals, plus duplicate duration nodes and missing/negative/overflow seconds.
The encrypted question/answer fixtures additionally cover bounded `unipus.`
framing, separate decryptions, exact Group/question binding, a mixed ordered
two-module simple Group with bounded per-child `type`/`replyType` judge labels,
and removal of key material. Their synthetic native
instance identities are positive numeric, matching the mutation shape
independently observed in the donors. Submission fixtures cover one accepted
version receipt plus single- and multi-module exact receipt-versioned
user-module readbacks. They use only synthetic answer values; tests require
Course/Group/version/question order/submitted-state/answer equality and reject
changed or reordered rows. A missing receipt produces Inconclusive without
making a transport request.

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
- Provider metadata and Authentication expose the same validated Capture recipe v1: `AssistedSession` reads same-snapshot `u-openid` and raw `Authorization` headers into one required `ProviderCompositeSession` JSON output, while ManualImport, CaptureTool and BrowserExtension remain validated non-renewable `Jwt` imports.
- Runtime schema validates conservative Provider/account concurrency and account scan defaults plus bounded Provider/account/Task overrides for residence seconds and video playback; mutation paths reject incomplete or stale-schema snapshots.
- Expired native Composite metadata may enter only the exact atomic Password renewal path; expired imported JWT cannot renew.
- Duplicate resources or Groups fail the complete scan.
- `courseInstanceId` remains operation-only and absent from serialized Tasks.
- Stable Group identity contains CourseResource, Unit and Group components.
- Fresh TaskDetail must rediscover that exact identity and reject disappearance or drift.
- Group question counts are optional bounded facts and never authorize a submission by themselves.
- Course point totals never imply Group completion.
- `base` labels never imply executable or assessment capability.
- Unknown tree roles fail closed.
- Progress duration remains untyped and independent from completion.
- Study-record duration seconds require one exact unique CourseResource/Unit/Group binding.
- Content and standard answers remain independent fresh documents; neither may be reused as submission evidence.
- Nested content encoded as JSON text or supplied as an inline object normalizes identically under one size/zeroization boundary; rich `text`/`pcText` and per-component stems are preserved without HTML markup.
- Short-answer standard-answer fallback accepts bounded plain analysis text, a top-level JSON analysis string and ordered child analysis rows, but never arbitrary JSON shapes.
- Decrypted byte owners zeroize on parse success and failure; raw parsed JSON owners redact Debug output and recursively zeroize nested string values after normalization.
- Executable answer-bearing and preset request JSON uses a zeroizing owner on every local success, error and cancellation path.
- Question snapshots remain bound to the exact stable CourseResource/Unit/Group Task and cannot build a draft for another route.
- Submission previews contain no selected answer values or executable provider payload.
- Bounded positive choice, multi-child composite choice, short-answer, translation/revise, fillblank/banked-cloze, ordering and matching Groups with one shared type or exact one-type-per-Question cardinality advertise execute/verify; composite option sets, matching-left and ordering child facts remain explicit.
- Multi-Question execution preserves draft positions, native module identities, per-module children and flattened judge/completion order; fresh count/type drift fails before mutation.
- Every Question execution requires one bounded content-derived current-donor judge descriptor per answer child and never substitutes the Group `base` when that exact metadata is absent.
- Submission execution requires a positive numeric native instance ID and rejects arbitrary read-only Question identities before transport.
- Codes `600001` and `600002` never produce a receipt and never trigger an implicit mutation retry.
- Ambiguous mutation transport failures return after exactly one Provider attempt.
- Completion execution uses the five preset labels, exact `exit-ticket`, or one exact single oral family only as candidates. It requires a fresh exact Unit/Group `tab_type=text|video` leaf for presets or `tab_type=task` for exit-ticket/oral, skips already-completed leaves, emits either the exact empty `submitType=2` body or oral's bounded `instanceId=0` placeholder body at most once, and exposes a goal-bound fresh-detail plus exact-progress verify-only path for Core recovery; its receipt and synthetic body are never completion evidence.
- Verification requires the receipt version and an exact fresh Course/Group/complete ordered Question set/submitted-answer readback.
- Missing receipts are Inconclusive; receipts alone never confirm success, score, progress or completion.
- Every Group advertises BrowserBridge, but a session spec is issued only after fresh exact Task rediscovery; the isolation key hashes account plus Task and the origin set is exactly `ucontent`/`ipub`.
- Rendered BrowserBridge fixtures must cover legacy `pc-slider`, Ant Tree/Menu and `u3menu` leaf discovery, Micro→Tab→Task ordering, known popup closure, pause/restart budget accounting, video playback ceilings and origin/frame/session-bound iframe SCAN/CLICK/result messages; wildcard messages and arbitrary selectors are rejected.
- Browser residence and final upload-answer fixture families remain required as their shared execution contracts land; their absence is a tracked implementation gap, not a first-batch deferral. Exit-ticket and exact single-oral-family execution boundaries are covered offline, pending live validation; mixed oral/upload compound shapes remain active work.
- Upload boundary coverage requires code-200 grant parsing, redacted/zeroizing token ownership, exact file-key retention, the donor's 4 KiB minimal MP3, bounded audio/mpeg artifacts, deterministic multipart framing and exact key equality in object-store readback. The grant and object upload are not treated as final answer or completion receipts.
- Discussion synthetic coverage now binds fresh Course/class/curricula/current-user facts, a unique first-page topic, bounded 20-row reply pages, a redacted/zeroizing immutable reply intent, accepted-but-unverified mutation receipts and exact current-user/content readback. Durable Draft/attempt registration and final Group completion remain shared integration work.
