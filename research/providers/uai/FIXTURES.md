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
```

They cover typed Password success/rejection/slider outcomes, strict atomic
openid/JWT parsing, bounded user-info identity validation, Course →
CourseResource flattening, paired point counts, fresh detail binding, redacted
Course-instance routing, the outer/nested Course-tree envelope,
Unit/Section/Micro/Group identity separation, identity-bound per-Unit flags plus
raw progress duration, and exact CourseResource/Unit/Group-bound learning
seconds. Inline negative tests cover malformed/extra composite
fields, duplicate
resources/groups, misbound details/contexts, unknown roles and impossible point
totals, plus duplicate duration nodes and missing/negative/overflow seconds.

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
```

Remove usernames, passwords, JWT/openid values, annotator tokens, user IDs,
class/school IDs, instance routes, real names, answers, scores, free-form
content, media URLs and request identifiers. Preserve only bounded structural
field names, placeholder identities and response/result codes.

## Regression requirements

- Numeric and string CourseResource IDs normalize identically.
- Duplicate resources or Groups fail the complete scan.
- `courseInstanceId` remains operation-only and absent from serialized Tasks.
- Stable Group identity contains CourseResource, Unit and Group components.
- Course point totals never imply Group completion.
- `base` labels never imply executable or assessment capability.
- Unknown tree roles fail closed.
- Progress duration remains untyped and independent from completion.
- Study-record duration seconds require one exact unique CourseResource/Unit/Group binding.
