# WELearn capability map

| Asterism capability | Primary evidence | Use | Current decision |
|---|---|---|---|
| Authentication | Fanyuchang 2026 | Reference | Native Password/OIDC, ImportedCookie, Capture-assisted Cookie and external-browser-OAuth Cookie validation are offline/native-boundary covered; native redirects bind the exact SSO transfer and authorize-callback paths, immutable Capture v4/v5 alternatives distinguish assisted versus external-provider acquisition while keeping captcha/SMS/OAuth state browser-local; scoped Cookies and typed captcha/SMS outcomes; live pending |
| Stored session validation | Fanyuchang 2026 | Reference | Core-scoped Cookie resolution, authenticated Course-list validation and atomic Password/Composite renewal are native-boundary covered; live pending |
| CourseInventory | Fanyuchang 2026 + YZBRH | Reference | Native authenticated `authCourse.aspx?action=gmc` read is implemented behind shared NetworkProfile; live pending |
| TaskInventory | Fanyuchang 2026 + YZBRH | Reference | Native Course page → POST `courseunits` with GET compatibility only after explicit 404/405 → one `scoLeaves` response per Unit is implemented all-or-nothing and emits the exact five audited read/execution/verification/duration capabilities; live pending |
| TaskDetail | Fresh Course/Unit/SCO inventory | FromScratch | Re-lists Courses and re-runs one complete Course scan, then requires exactly one matching stable SCO identity, identical normalized Task snapshots in both public and nested detail views, the exact versioned fingerprint derived from that snapshot, a public remote state matching normalized visibility/completion observations, and the complete inventory-owned capability set without duplicates or unevidenced extras; mixed, stale or contradictory injected/restored details fail closed before execution; live pending |
| TaskProgressRead | YZBRH | Reference | Implemented through a fresh non-mutating `getscoinfo_v7` CMI request; completion, progress, score and time observations are parsed independently; live pending |
| DurationRead | YZBRH + Fanyuchang 2026 | Reference | Independent fresh CMI reader maps donor-observed canonical integer `total_time` to bounded seconds, returns zero for explicit no-CMI/not-started state and fails closed on unknown grammar; live pending |
| ResourceExecution | Fanyuchang 2026 + YZBRH + Auto_WeLearn | Reference | Native standalone fresh rebind → optional completed preflight → `startsco160928` → optional `setscoinfo` → selected-score `savescoinfo160928` → optional current-donor second `crate=100` save → exact fresh CMI verification is implemented. An exact donor uninitialized marker is an absent pre-mutation baseline: zero-time/set-then-save and save-only plans continue, while fresh-time preservation fails before start. Settings expose fixed/uniform/clamped-Gaussian selection and the audited sequence/time/CMI/write/endpoint facts, while `WellearnResourceExecutionPlan::validate` rejects every cross-donor combination before TaskDetail or native I/O. Returned transport documents must also structurally match the frozen start/set/save shape before CMI parsing; boolean rejection receipts remain diagnostic and fresh CMI remains the sole success predicate. The only accepted complete shapes are current-donor standalone dual-save, current-donor score-100 fresh-time atomic final, legacy selected-score set/save, and Auto score-0 zero-time save-only atomic final. Both atomic final tuples remain encoded, native-testable and available to read-only verification, but singleton TaskExecution fails closed before transport until Core supplies one-start duration-completion authority. Ambiguous results never replay |
| DurationReport | YZBRH + Fanyuchang 2026 + Auto_WeLearn | Reference | Native fresh-read → donor-specific start → complete preservation baseline → bounded heartbeat → donor-specific finalization → strict fresh-read lifecycle is offline/native-boundary covered for YZBRH and Auto single-file singleton duration. The public frozen plan validates the 1..19,800-second target and exact client-counter=1/preserve-fresh=60/implicit-server=60 cadence before fresh detail, and native transport repeats the same check before session I/O. Returned documents must bind start, accepted/rejected heartbeat counts and protocol-specific final receipt presence to that plan before CMI parsing; boolean rejections remain diagnostic rather than success. The mutation-capable baseline recognizes YZBRH's exact `学习数据不正确` uninitialized marker, starts once and then requires strict CMI; a valid JSON response with no CMI does not start and fails before keep because Asterism will not synthesize donor defaults. Unrelated malformed reads fail closed and read-only capabilities never start. Current Fanyuchang duration and modular Auto duration are marked `AtomicDurationCompletion`; their exact one-session final completion mutation remains a shared Core contract gap rather than being approximated by split singleton executions. Current client-counter rejection stops later keeps without ambiguous replay; live validation remains pending |
| Assessment/exercise behavior | Fanyuchang 2026 + YZBRH + Auto_WeLearn | Reference | Audited donors do not expose Question/Answer endpoints; their exercise behavior is the direct SCO completion/progress/score preset mapped to `ResourceExecution`, now implemented offline/native-boundary; audit remains open to new evidence |
| Result verification | Fresh Course/SCO/CMI reads | FromScratch | DurationReport verifies preservation plus changed time; ResourceExecution implements goal-bound `verify_execution` over the frozen settings and exact completion/progress/score tuple, so Core recovery fresh-reads without replay |
| BrowserBridge / Capture | Fanyuchang 2026 Cookie mode + SSO browser implementation + 2026-08-13 public login audit | Reference | Capture recipes v4/v5 separate five exact navigation origins from the sole WELearn credential-read origin, distinguish assisted versus external-browser-OAuth acquisition, and require the current loader's exact `GET /ajax/authCourse.aspx?action=gmc` to receive `200 application/json` before resolving the Cookie. The helper enforces that response gate, so the audited anonymous `200 text/html` login script cannot complete Capture; native Course parsing remains final authority. Manual captcha/SMS and QQ/WeChat/Apple navigation are evidenced; exact third-party choice and callback live validation remain pending, while no Task BrowserBridge behavior exists in the donors |
| Unit/all-task bulk orchestration | Fanyuchang 2026 + YZBRH + Auto_WeLearn | Reference | Provider now exposes bounded Unit observations and a pure `WellearnBatchPlan` boundary that freezes parent Course identity, validates each flow's exact all/explicit shape (arbitrary ordered multi-Unit for Fanyuchang, response-order subset for modular Auto, one-or-all for YZBRH/Auto single-file), retains selected-but-empty Units, response-order SCO membership, separate Unit/raw-SCO/merged visibility, completion observations, donor-specific filtering, dispatch and target allocation, plus Auto's aggregate budget and discarded remainder. `validate_batch_plan_integrity` rejects internal drift among the frozen flow profiles, selection/Unit/entry facts and Auto target arithmetic both at publication and before rebind. `validate_fresh_batch_entry` then binds one frozen child ordinal to a complete fresh TaskDetail, rejects Course/SCO/selected-Unit or current donor-eligibility drift, and never reorders membership or recomputes targets when response positions move. Current Fanyuchang and YZBRH duration retain hidden/completed rows; YZBRH and Auto completion filter to the broad raw `iscomplete` contains `未` branch; modular Auto duration keeps visible SCOs and preserves floor targets including zero-second children and its evidenced 330-minute aggregate maximum. `AutoLegacyDuration` separately preserves the single-file route's visible-only, per-child random/fixed seconds, sequential dispatch and bare-save lifecycle. Shared Core/API still need a durable Unit/selection contract, zero-target child semantics and parent/child Execution creation/recovery that invokes this rebind from persisted authority; the Provider plan does not schedule or persist children |

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
readback. A strict fresh CMI read must show `completed`, progress `1` and the
exact final score. No
post-mutation failure is automatically replayed. `ExecutionVerify` reuses the
frozen runtime settings, performs another full TaskDetail identity rebind and
calls only the non-mutating fresh CMI read. It proves the exact selected tuple,
not generic Completed state, so ambiguous results and crash recovery never
repeat start/setscoinfo/save.

The write-mode setting keeps exercise `set_then_save` separate from Auto's
completion-bearing `save_only` final tuple. It encodes the latter for the
pending atomic boundary; a standalone ResourceExecution is not claimed as the
exact modular duration sequence. Each mutation response exposes an acceptance
boolean. A well-formed explicit rejection does not falsely prove failure or
suppress the donor's later independent save; exact fresh CMI remains the sole
success predicate. Network, authentication, malformed-response and other
ambiguous failures stop without replay and require recovery review.

Visibility remains an observation rather than a Provider capability ban. The
current Fanyuchang donor disables both visibility and completed checks; YZBRH
duration also keeps every row. YZBRH completion and Auto filter only the raw
SCO `isvisible=false` field, not Unit visibility, so a selected hidden Unit's
missing/true leaf remains eligible while the merged Task state honestly stays
`NotOpen`. The Provider retains separate Unit, SCO and merged visibility facts,
fresh-binds the same Course/SCO identity and never relabels the Task as open.
Its dispatcher opts into Core's exact-action NotOpen exception only for
ResourceExecution, DurationReport, or their audited composite; Core still
rejects Expired and Removed.

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

Provider settings encode Auto modular's implicit phase plus fixed score 0,
selected-score/save-only and zero-time final tuple, and current Fanyuchang's
client-counter plus fresh-time set/save tuple. `WellearnBatchExecutionShape`
marks both as atomic duration-completion children, while
`WellearnAtomicCompletionProfile` freezes Fanyuchang's single score-100
fresh-time set/save versus Auto's score-0 zero-time save-only final.
`WellearnAtomicDurationCompletionPlan` expands either profile and one frozen
target into the complete immutable cadence, duration protocol, score, CMI,
time, write, save-sequence and endpoint/Referer facts, and rejects every
restored cross-donor mixture. Auto's evidenced equal-floor zero-second target
is valid only in its atomic profile; current Fanyuchang still requires at least
one client-counter second. This Provider plan neither persists nor schedules an
operation. A separate `WellearnAtomicDurationCompletionDocuments` bundle keeps
initial, optional post-duration and final CMI evidence apart from ordered
start/heartbeat/set/save acceptance receipts. Its plan validator requires the
current donor's post-duration evidence, accepted start and terminal first keep
rejection, while Auto has exactly one receipt per complete 60-second interval,
no post-duration/set slot and supports an empty zero-target keep list. Explicit
negative receipts remain diagnostics; no bundle grants mutation authority.
An unregistered Provider-owned native transport now consumes only this
validated plan and emits the combined bundle in one session/route lifecycle.
It implements the exact request sequence and no-replay error boundary, but is
not present in Provider capability registration and cannot be reached from
singleton TaskExecution. Core must still persist, authorize and recover the
composite attempt before an execution entry may call it without inserting a
bare save or second start.

`verify_atomic_duration_completion` is a separate pure readback boundary over
the validated plan and combined documents. It requires exact
completed/progress-100/profile-score CMI for both flows. Current Fanyuchang
also requires both fresh session/total-time fields from the post-duration CMI
to remain byte-identical in final CMI; modular Auto verifies score 0 without
inventing a zero-time readback predicate that its save-only request does not
write. Receipt booleans never affect this goal predicate. The verifier performs
no I/O and still does not register or authorize atomic execution.

One donor-level orchestration contract remains a shared Core gap. The Provider
now freezes bounded Unit identity, all/explicit selection, explicit order,
selected-but-empty Units, donor flow, SCO membership and target facts. Core/API
must durably persist that plan and create/recover its child Executions rather
than asking the Provider to schedule them. Current Fanyuchang keeps
hidden/completed SCO rows and samples per-SCO targets; YZBRH also derives each
target independently; Auto_WeLearn filters visible SCOs, samples one aggregate
minute budget, floors one equal per-child share and discards the remainder.
Immutable Core Execution identity now makes donor-style per-Execution random
duration and uniform/clamped-Gaussian score selection retry-safe, and Core's
persisted capability-step plan still needs the atomic duration-completion
extension described above.
