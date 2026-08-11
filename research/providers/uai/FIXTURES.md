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
- Provider metadata accepts the transient `ProviderSpecific` Password candidate and the resulting persisted `Composite` replacement, while manual import remains `Jwt`.
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
- Decrypted byte owners zeroize on parse success and failure; raw parsed JSON owners redact Debug output and recursively zeroize nested string values after normalization.
- Executable answer-bearing and preset request JSON uses a zeroizing owner on every local success, error and cancellation path.
- Question snapshots remain bound to the exact stable CourseResource/Unit/Group Task and cannot build a draft for another route.
- Submission previews contain no selected answer values or executable provider payload.
- Only bounded positive `single-choice`, `multichoice` and `short_answer` Groups with one shared type or exact one-type-per-Question cardinality advertise execute/verify.
- Multi-Question execution preserves draft positions, native module identities, per-module children and flattened judge/completion order; fresh count/type drift fails before mutation.
- Multi-Question execution requires one bounded content-derived current-donor judge descriptor per answer child and never substitutes the Group `base` when that exact metadata is absent.
- Submission execution requires a positive numeric native instance ID and rejects arbitrary read-only Question identities before transport.
- Codes `600001` and `600002` never produce a receipt and never trigger an implicit mutation retry.
- Ambiguous mutation transport failures return after exactly one Provider attempt.
- Preset no-Question completion uses the five base labels only as candidates, requires a fresh exact Unit/Group `tab_type=text|video` leaf, skips already-completed leaves, emits the exact empty `submitType=2` body at most once, and requires Core progress-only verification/recovery; its receipt and synthetic body are never completion evidence.
- Verification requires the receipt version and an exact fresh Course/Group/complete ordered Question set/submitted-answer readback.
- Missing receipts are Inconclusive; receipts alone never confirm success, score, progress or completion.
