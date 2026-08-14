# Answer evidence and completion policy

This document defines shared Core ownership for reusable answer evidence and
post-submission task completion. Provider implementations supply audited reads,
mutations and verification facts; they do not own the corpus or either policy
state machine.

## Owner-global Answer Evidence Corpus

The reusable corpus is scoped by `owner_user_id`. It is not partitioned into
independent Provider-account, Course or Task caches. Evidence learned from any
account, platform, Course, Task or Attempt owned by the same user may be
considered by answer resolution for another Task only after conservative
question/content matching.

Every corpus assertion retains provenance instead of flattening reliable values
into an unqualified answer:

- Provider and source account;
- Course, Task, Question snapshot and Attempt when available;
- result page or other audited evidence surface;
- original answer source and candidate identity when applicable;
- evidence class (`Official` / `ProviderNative`, `VerifiedHistorical` or
  `Unverified`);
- observed and verified timestamps, score and per-Question judgement facts;
- exact and future semantic content fingerprints used for matching.

Evidence class and answer source are independent. An AI or manually sourced
answer may become `VerifiedHistorical` after exact per-Question verification;
it does not become `ProviderNative`. Conflicting assertions and all provenance
records remain visible. Core must not promote a task-level score or overall
completion flag into per-Question correctness.

The existing same-Task `LocalAnswerCache` remains a conservative exact cache.
It does not replace the owner-global corpus and does not silently broaden its
matching scope.

## Bootstrap harvest

The first successful authentication/binding of a ProviderAccount creates one
durable, idempotent bootstrap-harvest job by default. Its authority is read
only. It may enumerate every safely readable historical completed Task for that
account and collect bounded normalized Questions, official/reference answers,
the user's submitted answers, per-Question judgement, score and retake facts.

The job must never create an Attempt, unlock a Task, submit an answer, trigger a
retake or mutate remote state to reveal evidence. A missing permission, dynamic
browser requirement or unsupported history surface becomes an explicit partial
report or blocker. Crash recovery resumes bounded reads without duplicating
corpus provenance. Completion of the first scan prevents repeated full scans
unless an explicit rescan policy creates a new generation.

Normal execution is incremental: after `SubmissionVerify` obtains reliable
per-Question facts, Core atomically appends or merges those facts into the same
owner-global corpus. Binding and using more accounts therefore grows one corpus
without requiring repeated manual full scans.

## Strict Completion

Strict Completion is a distinct, default-enabled state machine whose only goal
is to reach the platform-defined `Completed` or `Passed` state for a Task point.
One accepted submission is not automatically completion. When an audited
platform permits another Attempt and fresh verification remains
`Pending`/`InProgress`/not-passed, Core may create a new Question snapshot,
Draft and Attempt, resolve again using current corpus and configured answer
sources, submit through the normal mutation ledger and verify independently.

The loop stops immediately when fresh remote state is `Completed` or `Passed`.
It also stops at any explicit attempt, elapsed-time or policy limit; remote
attempt/time/unlock/manual-review restrictions are hard bounds. Ambiguous
Attempt creation or submission is recovered and verified, never blindly
replayed. Core never fabricates progress or weakens the Provider's pass rule.

## Score Improvement

Score Improvement / Retake is a separate, default-enabled state machine entered
only after the Task is already `Completed`/`Passed` and only when the platform
explicitly allows another retake/redo. It may use official answers,
`VerifiedHistorical` evidence and configured resolvers to pursue a target score
within independent attempt/time/policy limits.

Failure to improve, exhausted retakes or a lower later score cannot change the
already established completion state back to incomplete. Disabling Score
Improvement cannot disable Strict Completion, and disabling Strict Completion
cannot implicitly authorize score retakes.

## Policy ownership

Both state machines are enabled by default, with conservative finite limits.
All settings have administrator/Owner-managed defaults. Where user, account,
Course or Task overrides are exposed, Core persists the resolved immutable
policy snapshot and its source attribution for each run. The Owner can change
defaults and administratively replace previously changed options through the
management surface; a policy edit affects new state-machine decisions and does
not rewrite historical Attempts or evidence.
