# Identity, answer evidence and completion policy

This document is the shared Core contract for identity ownership, reusable
answer evidence and post-submission completion. Provider implementations own
audited platform steps and wire details; they do not own either corpus layer or
the shared policy state machines.

## Multi-user identity invariant

Asterism remains multi-user. QQ Identity is part of Domain and Storage identity;
one QQ identity binds one independent Asterism `User`. That User owns their own
ProviderAccounts, Tasks, Executions, Secrets, raw QuestionSnapshots,
Submissions, Attempts and Audit records. A bot/server owner is a high-privilege
`Master` User, not the owner of every other User's platform accounts or private
learning data. Existing Master, Operator and User authorization boundaries stay
in force.

## Two evidence layers

Private Answer Evidence is strictly scoped by `owner_user_id`. It preserves the
complete raw Question and private provenance: ProviderAccount, Course, Task,
Attempt, submitted answer, result/readback, official/reference answer,
per-Question judgement, score, retake facts and timestamps.

The Server/Instance Global Answer Corpus contains normalized, verified and
de-identified reusable assets projected from Private Evidence. It can be used
across Users on the same Asterism instance, but ordinary consumers cannot read
source QQ identity, ProviderAccount, Course identity, Attempt or other private
provenance.

These are not independent competing answer banks. Every evidence assertion
reliable enough for normal AnswerResolve/cache reuse must eventually have an
idempotent Global Corpus projection:

- Provider-published Official / ProviderNative answers become positive global
  evidence;
- an actually submitted answer explicitly judged correct per Question becomes
  `VerifiedHistorical` positive evidence;
- an actually submitted answer explicitly judged wrong becomes
  `NegativeEvidence` and excludes that semantic candidate;
- an overall score or Task completion flag never creates per-Question evidence.

When a safe global Question identity cannot yet be built because parsing,
shared context/options, child hierarchy or semantic normalization is
incomplete, Core records `PendingCorpusProjection` / `UnmatchedEvidence`. It
retains the raw private assertion for later reprocessing instead of guessing a
merge, dropping it or leaving reliable evidence permanently in a local cache.
Projection is replay-safe and cannot double-count the same source evidence.

The existing owner/Task-scoped `LocalAnswerCache` remains a private exact-
fingerprint shortcut. It is not merged with the Global Corpus and cannot be the
long-term endpoint for Official, VerifiedHistorical or NegativeEvidence.

## Evidence history and arbitration

`AnswerSource` and evidence verification status are independent dimensions.
Official, ProviderNative, VerifiedHistorical, ExternalBank, AI and Manual facts
are not overwritten by a last-write-wins value. Core preserves answer versions,
`first_seen`, `last_seen`, `verified_at`, positive/negative counts,
superseded/changed facts and source categories. A later conflicting Official
answer raises the new fact's priority and invalidates or lowers older evidence
without deleting history.

The Global Corpus stores de-identified aggregate positive and negative evidence
and source classes. It does not expose identity-bearing private provenance.
Consensus among independent sources may raise confidence only when no stronger
evidence exists. Consensus cannot override Official evidence or vote a
candidate back into validity after explicit NegativeEvidence.

Core tracks empirical resolver accuracy from real per-Question readback, with
bounded dimensions such as QuestionKind, Provider and course category. Answer
selection may combine evidence quality, empirical accuracy, cost, latency and
declared confidence, but statistics never bypass authorization, assessment or
manual-review policy.

## Semantic Question identity

Cross-Attempt/Task/Course reuse requires conservative semantic identity and
answer rebinding. Evidence stores semantic option/child answers, not only
letters or positions. A current QuestionSnapshot is matched first, then the old
semantic answer is rebound to its actual current option/child identity. Shared
context, shared options, ordered children, reading material, attachments and
compound hierarchy are identity inputs.

Cross-Provider deduplication is allowed only with strict unique semantic
evidence. Similar stems or fuzzy/vector similarity alone cannot automatically
merge Questions. Ambiguous matches remain separate Corpus entries.

## Bootstrap and incremental ingestion

The first transition of each ProviderAccount into valid authenticated state
creates one default read-only bootstrap-harvest job. The job is durable,
idempotent, interruptible and recoverable, with persisted state, progress,
watermark and completion. Session renewal, Cookie rotation and later
reauthentication do not repeat a completed full scan. A User may explicitly
request a new full-scan generation; future incremental harvests are separate.

Bootstrap may enumerate safely readable historical completed Tasks and collect
Questions, shared context, official/reference answers, submitted answers,
per-Question correctness, score and retake facts. It never creates an Attempt,
unlocks a Task, submits, retakes or performs another mutation to reveal data.
Raw facts first enter Private Evidence and all reusable assertions are then
projected idempotently into the Global Corpus.

Normal operation is incremental. `SubmissionVerify` and any other reliable
result read atomically append Private Official, VerifiedHistorical and Negative
facts and enqueue/perform their Global projection. More Users, accounts and
verified Tasks therefore grow one privacy-safe instance corpus naturally.

## Answer and Draft policy

Answer policy supports source priority, minimum confidence, AI/external cost
budgets and explicit `LeaveBlank` / `ManualReview` fallback. Partial submission
coverage is calculated against the complete fresh QuestionSnapshot; unknown,
unsupported or unresolved Questions lower coverage and are never silently
removed from the denominator or filled with invented values.

Shared Draft semantics may express `DraftOnly`, `SaveOnly` and
`SubmitWhenCoverageReached` when audited Providers support them. Endpoint,
payload, DOM and exact recovery remain Provider-private. Immutable Drafts freeze
the complete snapshot partition and resolved policy. Attempt History records
every attempt's state, score, Question results, selected candidates/sources/
evidence, newly learned evidence and score delta.

The owner-scoped Task Attempt History index groups these facts by Execution
without copying the full Question payload into every list entry. It returns
each durable ExecutionAttempt, per-class counts of evidence learned by that
attempt, immutable Draft and QuestionSnapshot references, answer-source counts,
the SubmissionResult reference, verification/Question-result summary and a
normalized score delta from the preceding scored result on the same Task. The
existing bound Draft and Result resources remain the canonical full-detail
reads. History reads perform no Provider call and never expose another owner's
private evidence or provenance.

## Strict Completion

Strict Completion means fresh remote verification reaches platform-defined
`Completed` or `Passed`; a successful mutation or accepted receipt is not
completion. Core records a structured `CompletionDiagnosis`, including at least
`score_below_threshold`, `duration_insufficient`, `required_children_pending`,
`prerequisite_locked`, `teacher_review_pending`, `human_action_required`,
`unsupported_capability`, `protocol_drift`, `attempt_limit_reached`,
`window_closed` and `remote_unknown`.

Within audited, safely recoverable bounds, Strict Completion may default on for
mechanical work such as video, reading, duration, resource completion,
prerequisites and technical-failure recovery. It stops at Completed/Passed or an
explicit attempt/time/policy/remote limit. Ambiguous non-idempotent operations
are verified, never blindly replayed. Scored assessments must not use AI or the
Corpus in an unsupervised submit-until-pass loop; a failed assessment enters
review/confirmation or an explicitly controlled retry/retake policy.

Provider execution and submission verification may map audited, sanitized
platform facts into a shared `CompletionDiagnosis`. Core still owns the final
observation: only fresh verified `Completed` produces a completion outcome;
Provider diagnosis is ignored once completion is verified. A rejected
submission without a more precise audited diagnosis becomes
`score_below_threshold`; every other incomplete result without evidence becomes
`remote_unknown`, which stops automatic work instead of guessing a retry.

Each applied Strict Completion observation has an exactly-once ledger binding
to one Execution and its active ExecutionAttempt. Storage derives owner,
ProviderAccount, Task, assessment class and frozen policy from durable rows,
checks the live worker claim, and creates or advances the workflow in the same
transaction as the ledger entry. A replay cannot increment the workflow attempt
counter twice. `Stopped` ends automatic work but is not negative completion:
a later fresh Completed/Passed observation monotonically closes the workflow as
Completed and clears the obsolete diagnosis.

A Formal Strict Completion retry, and every retry of an Active scored
Submission workflow, is authorized by a durable record bound to one new
Execution, the exact Strict workflow ID and expected revision, the confirming
owner and confirmation time. A request boolean is not authority. The
scheduling transaction requires the workflow to remain owner/Task-bound,
Active and already attempted, and rejects missing or stale confirmation before
any Provider work. This exact confirmation is the only Strict retry path which
may move a HumanRequired Task back to Scheduled. For SubmissionExecute, the
new immutable Draft and its QuestionSnapshot must both have been created after
the workflow's prior observation; an unused Draft over an old Snapshot is not
a fresh attempt. The worker derives confirmation only from the persisted
Execution binding when applying the next observation.

## Score Improvement / Retake

Score Improvement is separate from Completion and is explicit opt-in after the
Task is already Completed/Passed. Before a retake, Core reads and freezes the
Provider's `RetakeScorePolicy` (highest score, last attempt, average, teacher
rule or unknown). It never assumes a retake is harmless.

Each authorized retake gets a fresh QuestionSnapshot, Draft, Attempt, mutation
ledger and verification. It prefers current Official and VerifiedHistorical
evidence within answer policy. It is finitely bounded and not an unbounded
background loop. Failure to improve, a lower later score or unavailable retake
cannot reverse an already verified completed Task.

## Policy ownership and snapshots

Product profiles may define Conservative, Balanced and StrictCompletion
defaults. Administrator/Master-managed defaults and permitted User,
ProviderAccount and Task overrides remain within existing authorization. Core
uses the established Provider default -> ProviderAccount override -> Task
override -> immutable Execution settings snapshot rule, records source
attribution, and does not rewrite historical Attempts when policy changes.

Every registered Provider schema receives the same Core-owned completion
fields at registration: Strict Completion enabled, Score Improvement enabled,
the two attempt limits, Score Improvement target and the two workflow time
limits. All seven may inherit through Provider, ProviderAccount and Task scope.
The canonical defaults enable Strict Completion, keep Score Improvement
disabled until explicit opt-in, allow three completion attempts and one score
attempt, target 100%, and bound the workflows to seven days and one day
respectively. Formal scored retry confirmation is a fixed safety invariant,
not a setting that an owner or lower scope can disable.

An Execution atomically persists both its immutable resolved settings and the
derived `CompletionPolicySnapshot`, including source revisions, capture time and
absolute deadlines. Migration of a durable snapshot that predates these Core
fields freezes the canonical defaults against its original capture time.
Missing Provider-owned values, unknown values, malformed types and
schema-version changes remain fail-closed; compatibility handling never
broadens Provider authority.

## Course aggregate progress

Course progress is an owner-scoped read model over persisted Course, Task and
verified Submission facts. Remote `Completed` is the completion authority;
local Execution success alone does not increment completed Task count. Removed
Tasks remain visible in the total discovered count but are excluded from the
countable completion denominator. Remaining, NotOpen, CreditBlocked,
HumanRequired and Failed counts are separate diagnostic dimensions and may
overlap where the underlying facts overlap.

The aggregate returns a `0..1000` completion ratio only when the countable
denominator is non-zero. It summarizes only each Task's latest verified scored
result and averages normalized score ratios so different raw point scales do
not distort the Course score. Required-task and duration aggregates are typed
optional dimensions: until audited Provider or dependency facts establish
them, they remain null rather than being reported as zero. The read performs no
Provider call and never crosses ProviderAccount ownership.

## Account health

Account Health is an owner-scoped Core read model, separate from any one
Provider's authentication wire. Persisted `AuthState` maps to Healthy,
Checking, ExpiringSoon, Expired, HumanActionRequired, Broken or
ProtocolChanged without treating an in-progress credential exchange as a
failure. HumanActionRequired retains the typed QR, SMS, callback, import or
other intervention reason.

An attempt-bound `protocol_drift` failure may override otherwise healthy
authentication only when its observed time is at or after the account's latest
state update. Reauthentication or repair therefore supersedes stale drift;
ordinary task failure never becomes account-level protocol change. The health
read exposes the exact Execution and timestamp for a live drift diagnosis,
performs no Provider call and remains bound to the account owner. Authentication
fallback execution remains a separate Provider/Core recovery workflow.

## Protocol Observation Inbox

Unknown protocol shapes remain fail-closed, but Core may retain a bounded,
sanitized observation for later donor audit. An observation contains Provider,
shared protocol surface, drift kind, a digest-bound JSON shape, first/last seen
times, occurrence count and an optional same-Provider Execution reference. It
never stores an original response body, credential, Cookie, authorization
header or other secret-shaped field.

The shape digest groups equivalent drift. A separate occurrence digest makes a
single event replay idempotent, so worker recovery cannot inflate the count;
new occurrences increment the aggregate transactionally. Execution provenance
is accepted only when its Task's ProviderAccount matches the declared
Provider. The instance inbox is Master-only and filterable by Provider and kind.
Provider call sites still own the decision to emit a redacted shape after they
fail closed; adding the durable inbox does not make unconnected drift paths
observable by itself.
`ProviderError` therefore exposes an optional validated shape payload rather
than asking Core to parse diagnostic text. During an Execution, Core persists
that payload before applying the existing failure or verification-recovery
policy and binds replay identity to the exact job and Execution. The daemon
configures this sink by default; a storage failure remains visible and retry-
safe instead of silently losing the observation. Providers attach the payload
only where their parser has an explicit structural fact.
Inventory scans use the same recorder even though no Task Execution exists yet.
Their occurrence identity is account-and-correlation bound and the persisted
Execution reference stays null. This allows unknown remote Task types found
during ordinary discovery to enter the same review inbox without manufacturing
an Attempt or weakening the scan's all-or-nothing ingestion boundary.
SubmissionBuild also precedes an Execution. Its drift occurrence is therefore
bound to the Task, immutable QuestionSnapshot and request correlation, retains
a null Execution reference and must be durable before the Provider failure is
returned. Candidate selection and coverage validation still happen first, and
the failure path never persists a partial Draft.
Question inventory and parsing use the same rule: a failed all-or-nothing read
may create a shape observation but never a partial QuestionSnapshot. Durable
pre-Question operations first persist an issued failure as Ambiguous and only
then record its shape, preserving the no-replay boundary even if Inbox storage
itself fails.
Fresh owner reads of progress and duration also feed the inbox when their
Provider reports a safe structural shape. These calls remain read-only and
Task/account bound; recording a shape does not update local completion state,
duration, score or evidence.
TaskDetail behaves the same way: a safe field-shape observation may be retained
after the authorized Provider call fails, but it cannot update the persisted
Task or expose route text, remote URLs or raw detail through the Inbox.
