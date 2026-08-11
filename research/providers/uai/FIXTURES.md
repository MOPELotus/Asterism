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
  answers/standard-multiple-choice.json
  submissions/accepted.json
  submissions/verified.json
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
framing, separate decryptions, exact Group/question binding and removal of key
material. Their synthetic native instance identity is positive numeric, matching
the mutation shape independently observed in both donors. Submission fixtures cover one accepted version receipt and an exact
receipt-versioned user-module readback. They use only synthetic answer values;
tests require Course/Group/version/question/submitted-state/answer equality and
reject changed answers. A missing receipt produces Inconclusive without making
a transport request.

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
- Only one-question `single-choice`, `multichoice` and `short_answer` Groups advertise execute/verify.
- Submission execution requires a positive numeric native instance ID and rejects arbitrary read-only Question identities before transport.
- Codes `600001` and `600002` never produce a receipt and never trigger an implicit mutation retry.
- Ambiguous mutation transport failures return after exactly one Provider attempt.
- Preset no-Question completion uses the five base labels only as candidates, requires a fresh exact Unit/Group `tab_type=text|video` leaf, skips already-completed leaves, emits the exact empty `submitType=2` body at most once, and requires Core progress-only verification/recovery; its receipt and synthetic body are never completion evidence.
- Verification requires the receipt version and an exact fresh Course/Group/question/submitted-answer readback.
- Missing receipts are Inconclusive; receipts alone never confirm success, score, progress or completion.
