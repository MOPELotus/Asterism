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
