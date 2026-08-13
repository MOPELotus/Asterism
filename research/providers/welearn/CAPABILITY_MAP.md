# WELearn capability map

| Asterism capability | Primary evidence | Use | Current decision |
|---|---|---|---|
| Authentication | Fanyuchang 2026 | Reference | Native Password/OIDC, ImportedCookie, Capture-assisted Cookie and external-browser-OAuth Cookie validation are offline/native-boundary covered; immutable Capture v4/v5 alternatives distinguish assisted versus external-provider acquisition while keeping captcha/SMS/OAuth state browser-local; scoped redirects/Cookies and typed captcha/SMS outcomes; live pending |
| Stored session validation | Fanyuchang 2026 | Reference | Core-scoped Cookie resolution, authenticated Course-list validation and atomic Password/Composite renewal are native-boundary covered; live pending |
| CourseInventory | Fanyuchang 2026 + YZBRH | Reference | Native authenticated `authCourse.aspx?action=gmc` read is implemented behind shared NetworkProfile; live pending |
| TaskInventory | Fanyuchang 2026 + YZBRH | Reference | Native Course page → POST `courseunits` with GET compatibility only after explicit 404/405 → one `scoLeaves` response per Unit is implemented all-or-nothing; live pending |
| TaskDetail | Fresh Course/Unit/SCO inventory | FromScratch | Re-lists Courses and re-runs one complete Course scan, then requires exactly one matching stable SCO identity; live pending |
| TaskProgressRead | YZBRH | Reference | Implemented through a fresh non-mutating `getscoinfo_v7` CMI request; completion, progress, score and time observations are parsed independently; live pending |
| DurationRead | YZBRH + Fanyuchang 2026 | Reference | Independent fresh CMI reader maps donor-observed canonical integer `total_time` to bounded seconds, returns zero for explicit no-CMI/not-started state and fails closed on unknown grammar; live pending |
| ResourceExecution | Fanyuchang 2026 + YZBRH + Auto_WeLearn | Reference | Native fresh rebind → optional completed preflight → `startsco160928` → optional `setscoinfo` → selected-score `savescoinfo160928` → optional current-donor second `crate=100` save → exact fresh CMI verification is implemented. Settings expose fixed/uniform/clamped-Gaussian selection, selected-score/current-dual-save sequences, set-then-save/save-only mutation families, normal zero-time versus duration-then-completion fresh-time semantics, plain-JSON versus `[INTERACTIONINFO]` CMI envelopes, and the current query-uid/full/simple-Referer versus historical plain/minimal/task-Referer mutation profiles separately. Explicit negative receipts are recorded and later donor mutations continue; ambiguous results never replay. Current Fanyuchang deliberately includes hidden/NotOpen SCOs, so Provider advertises and fresh-rebinds that route while preserving the honest remote state; shared exact-action admission is integrated and live behavior remains pending |
| DurationReport | YZBRH + Fanyuchang 2026 + Auto_WeLearn | Reference | Native fresh-read → donor-specific start → complete preservation baseline → bounded real-time heartbeat → donor-specific finalization → strict fresh-read lifecycle is offline/native-boundary covered. Settings expose YZBRH preserve-fresh, current donor client-counter and Auto implicit-server wire plans plus fixed/bounded-random duration; endpoint/start payload, keep fields, YZBRH fixed request gaps and receipt-continuation behavior follow the selected evidenced mode. Current client-counter `ret` outside `0/1` is recorded as an explicit rejection and stops later keeps before fresh verification, matching the donor without treating an unambiguous result as a replay hazard; fresh verification remains authoritative and live validation is pending |
| Assessment/exercise behavior | Fanyuchang 2026 + YZBRH + Auto_WeLearn | Reference | Audited donors do not expose Question/Answer endpoints; their exercise behavior is the direct SCO completion/progress/score preset mapped to `ResourceExecution`, now implemented offline/native-boundary; audit remains open to new evidence |
| Result verification | Fresh Course/SCO/CMI reads | FromScratch | DurationReport verifies preservation plus changed time; ResourceExecution implements goal-bound `verify_execution` over the frozen settings and exact completion/progress/score tuple, so Core recovery fresh-reads without replay |
| BrowserBridge / Capture | Fanyuchang 2026 Cookie mode + SSO browser implementation + 2026-08-13 public login audit | Reference | Capture recipes v4/v5 separate five exact navigation origins from the sole WELearn credential-read origin, distinguish assisted versus external-browser-OAuth acquisition, and require the current loader's exact `GET /ajax/authCourse.aspx?action=gmc` to receive `200 application/json` before resolving the Cookie. The helper enforces that response gate, so the audited anonymous `200 text/html` login script cannot complete Capture; native Course parsing remains final authority. Manual captcha/SMS and QQ/WeChat/Apple navigation are evidenced; exact third-party choice and callback live validation remain pending, while no Task BrowserBridge behavior exists in the donors |
| Unit/all-task bulk orchestration | Fanyuchang 2026 + YZBRH + Auto_WeLearn | Reference | Provider now exposes a pure bounded `WellearnBatchPlan` boundary that freezes response-order SCO membership, separate Unit visibility and raw SCO visibility observations, merged visibility, completion observations, donor-specific filtering, explicit dispatch and target-allocation semantics, and Auto's one aggregate budget plus discarded remainder. Current Fanyuchang and YZBRH duration retain hidden/completed rows; YZBRH and Auto completion filter to the donor's broad raw `iscomplete` contains `未` branch and resolve fixed/random targets per child; Auto duration keeps visible SCOs and preserves donor-exact floor targets, including zero-second children when the aggregate is smaller than the selected count; Auto's legacy single-file route is covered by the Fanyuchang-compatible profiles. Shared Core/API still need first-class Unit/selection scopes, zero-target child semantics and durable parent/child Execution creation/recovery; the Provider plan does not schedule or persist children |

## Implementation checkpoint

The initial fixture parser was intentionally small, but it is not a Provider
completion boundary. Current implemented code covers:

1. parse a bounded Course list into `RemoteCourse`;
2. parse Unit and SCO-leaf responses into `RemoteTask`;
3. keep SCO completion and donor-labelled learning time as separate facts,
   while the independent CMI-backed `DurationRead` accepts only the
   donor-observed canonical integer-second `total_time` grammar;
4. authenticate with Password/OIDC, ImportedCookie or Capture-assisted Cookie,
   and renew stored Composite sessions through Core's credential lifecycle;
5. execute the independent DurationReport lifecycle;
6. execute the donor-audited completion/progress/score preset through
   `ResourceExecution`, including fixed and bounded-random score configuration.

The crate now provides this parser boundary and synthetic fixtures. It also
implements Password/ImportedCookie/AssistedSession/ExternalBrowserOauth orchestration behind
injected transport and stored-session resolver contracts. Metadata remains
`Development` and advertises Authentication, CourseInventory, TaskInventory,
TaskDetail, TaskProgressRead, DurationRead, ResourceExecution,
ExecutionVerify and DurationReport. A registry-consistent development entry
can be composed from injected boundaries. A shared-policy,
non-redirecting native Password/OIDC and Course/Task HTTP transports now exist.
Stored credential resolution now validates exact account/reference/purpose,
session kind, expiry and UTF-8 shape before constructing a Cookie session.
Daemon registration is available only through an explicit disabled-by-default
development flag and requires a configured SecretStore. Password/Composite
renewal uses Core compare-and-replace and retries one read operation only after
an Authentication failure. Fresh CMI reads re-resolve the Course route, post
only `getscoinfo_v7`, and never start or mutate a SCO as a fallback. Raw CMI
times stay out of generic TaskProgress; the independent DurationRead
capability normalizes the donor-observed integer `total_time` seconds. Fresh
TaskDetail re-discovers the selected Course and exact SCO from the same
complete inventory path; disappeared identities return RemoteChanged and
versioned fingerprints satisfy the Core detail boundary. All live validation
remains pending.

`DurationReport` remains an independent mutation capability. Master-owned
runtime settings provide platform defaults plus account/task overrides for the
fixed/bounded-random report length, heartbeat interval and donor wire mode,
while Core-owned
concurrency and scan-interval behaviors stay explicit. The native lifecycle
keeps one resolved
Cookie and fresh Course route through baseline read, donor-specific start,
bounded heartbeats and finalization. YZBRH sends an initial preserved-state
keep plus complete-interval keeps and a preserved save; current Fanyuchang
always starts and sends exactly one client-counter keep per second without a
duration final save; Auto always starts, sends delayed implicit 60-second keeps
and a bare save. Every mode preserves completion, progress, score and success
status, keeps its duration wire mutations independent from the completion
`setscoinfo` boundary, and re-reads CMI before returning a verified outcome.
Missing baseline/readback preservation fields fail closed
instead of becoming synthetic defaults. Authentication before mutation may
renew once; no mutation is replayed after an authentication failure.
DurationRead remains independent from this write lifecycle and never starts a
missing CMI session.

The runtime schema accepts bounded one-second granularity for report length,
while heartbeat cadence exposes only the audited values: 1 second for current
client-counter and 60 seconds for preserve-fresh/implicit-server. This keeps
the current donor's short 10–30 second sessions expressible without inventing
unsupported keep schedules. Provider and
account execution concurrency are both expressible through 100, matching the
Auto_WeLearn UI and worker boundary; defaults remain 1 and Core retains its
global admission and lease controls.

`ResourceExecution` is separately advertised for every returned SCO. It freezes a
fixed score or selects one value from the configured uniform or
clamped-Gaussian bounded range using immutable Execution identity, rebinds the
exact Course/SCO through fresh detail, then uses the donor-audited start →
setscoinfo → selected-score save sequence. The CMI-envelope setting emits
current Fanyuchang's plain JSON or YZBRH/Auto_WeLearn's exact JSON plus
`[INTERACTIONINFO]` suffix. An independent sequence setting can
also reproduce current Fanyuchang's immediately following second save with
`crate=100`; in that mode the verified final goal is 100 while the selected
intermediate score stays visible in the sanitized outcome. A separate time
setting keeps the normal donor completion CMI's zero `session_time` and
`total_time`, or carries the fresh post-duration values into `setscoinfo` like
current Fanyuchang's duration-then-completion path. The latter requires both
fresh values before the first mutation and compares both against the immediate
readback. A strict fresh CMI
read must show `completed`, progress `1` and the exact final score. No
post-mutation failure is automatically replayed. `ExecutionVerify` reuses the
frozen runtime settings, performs another full TaskDetail identity rebind and
calls only the non-mutating fresh CMI read. It proves the exact selected tuple,
not generic Completed state, so ambiguous results and crash recovery never
repeat start/setscoinfo/save.

The write-mode setting keeps exercise `set_then_save` separate from
Auto_WeLearn duration completion's `save_only`. Each mutation response exposes
an acceptance boolean. A well-formed explicit rejection does not falsely prove
failure or suppress the donor's later independent save; exact fresh CMI remains
the sole success predicate. Network, authentication, malformed-response and
other ambiguous failures stop without replay and require recovery review.

Visibility remains an observation rather than a Provider capability ban. The
current Fanyuchang donor has both visibility and already-completed skip checks
explicitly disabled before it builds the execution list, so hidden SCOs retain
`NotOpen` while still advertising ResourceExecution, ExecutionVerify and
DurationReport. The Provider validates that a fresh boolean visibility fact is
still present and binds the same Course/SCO identity; it does not relabel the
SCO as open. Its dispatcher opts into Core's exact-action NotOpen exception
only for ResourceExecution, DurationReport, or their audited composite; Core
still rejects Expired and Removed.

The shared Engine now evaluates a DurationReport-only execution against the
duration goal instead of requiring the remote Task to become Completed. A
verified Pending, InProgress or Completed readback can therefore finish the
Execution, while every Provider error goes directly to HumanRequired without a
retry. Recovery of an abandoned duration report never calls TaskProgress and
never re-enters the write lifecycle. This closes the offline Core contract gap;
live Provider validation remains pending. Intermediate parser, Native HTTP and
DurationReport milestones are checkpoints only; they are not stopping
conditions. Capture/BrowserBridge and further assessment families remain in
scope whenever donor or sanitized live evidence establishes their protocol.

The durable duration-then-completion composite can also express Auto_WeLearn's
time worker final state with implicit-server duration, fixed score 0,
selected-score sequence and explicit zero-time completion mode. Current
Fanyuchang's corresponding client-counter path uses score 100; the default
completion-time `auto` observes Core's immutable plan/position and selects
fresh-time for that exact second step. Current mutation authority remains the
singleton step, and malformed plan bindings fail before transport.

One donor-level orchestration contract remains a shared Core gap. The donors
accept selected Units or all Units as a batch, but their membership and target
rules differ. Current Fanyuchang keeps hidden/completed SCO rows and samples
per-SCO targets; YZBRH also derives each target independently; Auto_WeLearn
filters visible SCOs, samples one aggregate minute budget, floors one equal
per-child share and discards the integer-division remainder. Unit identity,
donor-flow selection, frozen batch membership, derived child targets and
crash-safe parent/child Execution creation belong in Core/API rather than a
Provider-private scheduler.
Immutable Core Execution identity now makes donor-style per-Execution random
duration and uniform/clamped-Gaussian score selection retry-safe, and Core's
persisted capability-step plan covers the duration-then-completion composite.
