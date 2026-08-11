# Cidaren fixture plan

No real account was contacted during the 2026-08-11 audit. Initial fixtures are
synthetic reconstructions of fields read by frozen donor code and corroborated
by the public issue tracker.

## Initial synthetic fixtures

```text
fixtures/providers/cidaren/
  auth/token-success.json
  auth/token-rejected.json
  tasks/class-task-page-1.json
  tasks/class-task-page-2.json
  tasks/study-task-list.json
```

The class fixture includes multiple Courses, learning and test rows, pending,
in-progress, completed and expired states, `task_id=-1`, page boundaries and
unknown fields that must be dropped. The study fixture adds selected-Course
binding, ordinary `task_type=3` units, stable `list_id`, access flags and
repeated uninitialized task identity behavior.

These fixtures now drive the Development crate's strict token-response,
Course merging, stable release/list identity, state classification, redaction,
all-or-nothing pagination, selected-Course query binding, fresh TaskDetail and
TaskProgressRead tests. They remain synthetic and do not establish live
compatibility; native HTTP request and response boundaries are covered
separately with deterministic transports.

Inline negative tests must cover empty/oversized/non-JSON responses, non-object
records, unsupported task types/status values, duplicate release identities,
conflicting duplicate Course titles, inconsistent totals, missing pages,
oversized page/row counts, invalid percentages and unsafe remote components.
Legacy decoder tests generate synthetic JSON/base64 and insert the exact
versioned confusion bytes in memory; no published or real word/question payload
is copied into the repository.

## Required live-sanitized fixtures

After WebUI and Asterism-Plugin/Yunzai are complete and the user supplies a
read-only account:

```text
fixtures/providers/cidaren/
  auth/token-success-live.json
  auth/token-expired-live.json
  courses/class-task-empty-live.json
  tasks/class-task-mixed-live.json
  tasks/class-task-task-id-minus-one-live.json
  tasks/study-task-selected-live.json
  tasks/study-task-task-id-minus-one-live.json
  progress/class-task-fresh-live.json
  progress/study-task-fresh-live.json
```

Remove UserToken, browser crypto context, student name/code, school/class,
teacher identifiers, request signatures, exact timestamps, free-form personal
task names and all answer/topic material. Preserve bounded structural fields,
placeholder identities, result codes, status values and pagination shape.

## Regression requirements

- Imported tokens never appear in Debug, fixture, normalized metadata or logs.
- Class-task pagination is complete and total-consistent before normalization.
- Course identity is `course:{course_id}` and duplicate rows must agree.
- Task identity is `class-task:{release_id}` even when `task_id == -1`.
- Ordinary study identity is `study-task:{course_id}:{list_id}` even when
  `task_id == -1`.
- Selected Course, query and `StudyTask/List` response/rows must agree.
- TaskDetail and TaskProgressRead re-scan and reject missing fresh identities.
- Learning and test tasks remain distinct source types but both are Routine.
- Expiry, completion, progress, score and raw time remain independent facts.
- Raw `time_spent` never becomes `duration_seconds` without live unit proof.
- Legacy response decoding is bounded, exact-version only and rejects
  Capture-bound `jv=99` without retaining plaintext buffers.
- Unknown response fields are dropped.
- No Capture, answer, mutation or live-verification capability is advertised.
