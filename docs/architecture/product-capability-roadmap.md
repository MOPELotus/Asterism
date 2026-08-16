# Product capability roadmap

This roadmap records shared product semantics accepted for future Core slices.
An item becomes a shared abstraction only when multiple Providers expose
sufficiently consistent semantics. Platform endpoints, steps, payloads, DOM and
recovery details remain in Provider code: unify platform-level intent, not
Provider steps.

## Release sequencing

The first pre-release is a near-term delivery milestone after both of these
conditions are satisfied:

- real-account login, Session and account binding work through the public
  product path, with at least the safe read-only validation required by the
  declared release scope;
- the public delivery surfaces are usable: generated OpenAPI client, WebUI and
  Asterism-Plugin/Yunzai call the same Core actions and expose Development
  Provider limitations honestly.

The first pre-release does not wait for the second Provider batch and does not
claim that every mutation live gate or every Development Provider is Verified.
Its API compatibility promise is explicitly pre-release scoped. The second
batch (`zhihuishu`, `zjy`, `icve`), later Provider batches and final stable API /
OpenAPI compatibility freeze are internally classified as **Further Future**
and are not implicit dependencies of the first pre-release.

## Progress, dependencies and goals

- `CourseAggregateProgress` / `UnitAggregateProgress` represent total and
  required completion, duration, score, pending, blocked and HumanRequired
  counts.
- `TaskDependencyGraph` persists prerequisite, required-child and unlock edges
  plus structured blocker reasons. `NotOpen` alone is not enough.
- Remote availability facts include open/due/close windows and Attempt
  availability and participate in scheduling.
- `CourseGoalPlanner` raises intent from one Task to all required or all
  eligible Tasks in a Course. Whole-course and multi-course parent Executions
  retain independent child Attempts, mutation receipts, verification and
  recovery.

## Scheduler

Smart priority considers deadline, required/prerequisite impact, remaining
work, failure count, HumanRequired state, Provider rate/concurrency limits and
execution cost. Completing a prerequisite triggers fresh related discovery so
newly unlocked work can continue without waiting for the next periodic scan.

## Account health and authentication

`AccountHealth` includes Healthy, ExpiringSoon, Expired,
HumanActionRequired and Broken/ProtocolChanged. Core records an explicit
Provider-supported fallback chain such as stored session -> safe renewal ->
BrowserBridge/Capture -> HumanRequired. Recovery decisions are observable and
audited rather than improvised after execution failure.

## Human action and execution control

Captcha, SMS, face check, OAuth, Exam Code and similar intervention enter one
Human Action Inbox bound to the original Execution, Attempt, BrowserSession and
ProviderAccount plus a bounded recovery continuation. Completion resumes the
same work rather than restarting from step one.

Execution surfaces support Pause, Resume and Cancel. Cancel prevents future
actions and never claims to undo an already committed remote mutation. Dry Run
/ Plan Preview performs no mutation and shows discovered work, expected steps,
blockers/HumanRequired items, potential answer sources and Corpus hits,
coverage, external/AI calls and mutation boundaries.

## Protocol observation

Unknown Question kinds/type codes, result DOM, Task types, fields, endpoints or
versions fail closed and may create a bounded sanitized `ProtocolObservation`.
It records Provider, capability, structured shape, redacted JSON/DOM fragments,
digests, first/last occurrence and count under secret/PII rules. Developers can
review observations and promote them to fixtures/regression tests. This inbox
complements periodic full upstream sweeps.

## Notifications and analytics

Notifications cover new Tasks, Task/Course completion, authentication failure,
HumanRequired, approaching deadlines, unsatisfied completion, Attempt limits,
ProtocolDrift and unknown Question types. Digests may report completed, waiting,
failed/blocked and newly learned Official/Verified/Negative counts.

Analytics derive from structured Execution, Result, Corpus and Audit data:
Course completion, Task success/failure, CompletionDiagnosis distribution,
average Attempts, Corpus size/classes/hit rate, resolver empirical accuracy,
AI/external volume and cost, and Provider/capability drift. These signals may
inform source ranking, scheduler priority and Provider development priority but
cannot override explicit permission, assessment or safety policy.
