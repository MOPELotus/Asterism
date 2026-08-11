# WELearn capability map

| Asterism capability | Primary evidence | Use | Current decision |
|---|---|---|---|
| Authentication | Fanyuchang 2026 | Reference | Native Password/OIDC, ImportedCookie and Capture-assisted Cookie validation are offline/native-boundary covered; scoped redirects/Cookies and typed captcha/SMS outcomes; live pending |
| Stored session validation | Fanyuchang 2026 | Reference | Core-scoped Cookie resolution, authenticated Course-list validation and atomic Password/Composite renewal are native-boundary covered; live pending |
| CourseInventory | Fanyuchang 2026 + YZBRH | Reference | Native authenticated `authCourse.aspx?action=gmc` read is implemented behind shared NetworkProfile; live pending |
| TaskInventory | Fanyuchang 2026 + YZBRH | Reference | Native Course page → `courseunits` → one `scoLeaves` response per Unit is implemented all-or-nothing; live pending |
| TaskDetail | Fresh Course/Unit/SCO inventory | FromScratch | Re-lists Courses and re-runs one complete Course scan, then requires exactly one matching stable SCO identity; live pending |
| TaskProgressRead | YZBRH | Reference | Implemented through a fresh non-mutating `getscoinfo_v7` CMI request; completion, progress, score and time observations are parsed independently; live pending |
| DurationRead | YZBRH + Fanyuchang 2026 | Reference | Independent fresh CMI reader maps donor-observed canonical integer `total_time` to bounded seconds, returns zero for explicit no-CMI/not-started state and fails closed on unknown grammar; live pending |
| ResourceExecution | Fanyuchang 2026 + YZBRH + Auto_WeLearn | Reference | Native fresh rebind → optional completed preflight → `startsco160928` → `setscoinfo` → `savescoinfo160928` → exact fresh CMI verification is implemented with fixed or bounded random score settings; live pending |
| DurationReport | YZBRH + Fanyuchang 2026 + Auto_WeLearn | Reference | Native fresh-read → optional start → complete preservation baseline → bounded real-time heartbeat → preserve-and-finalize → strict fresh-read lifecycle is offline/native-boundary covered, with fixed or bounded-random duration settings; live pending |
| Assessment/exercise behavior | Fanyuchang 2026 + YZBRH + Auto_WeLearn | Reference | Audited donors do not expose Question/Answer endpoints; their exercise behavior is the direct SCO completion/progress/score preset mapped to `ResourceExecution`, now implemented offline/native-boundary; audit remains open to new evidence |
| Result verification | Fresh Course/SCO/CMI reads | FromScratch | DurationReport verifies preservation plus changed time; ResourceExecution implements goal-bound `verify_execution` over the frozen settings and exact completion/progress/score tuple, so Core recovery fresh-reads without replay |
| BrowserBridge / Capture | Fanyuchang 2026 Cookie mode + authentication outcomes | Reference | Capture recipe v1 and AssistedSession Cookie submission/validation are implemented for browser-obtained sessions. Current donors contain no reliable automated captcha/SMS solver or task BrowserBridge protocol; typed HumanRequired remains for those challenges pending sanitized evidence, not policy deferral |

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
implements Password/ImportedCookie/AssistedSession orchestration behind
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
fixed/bounded-random report length and heartbeat interval, while Core-owned
concurrency and scan-interval behaviors stay explicit. The native lifecycle
keeps one resolved
Cookie and fresh Course route through baseline read, optional start, bounded
heartbeats and finalization. It preserves completion, progress, score and
success status, does not call `setscoinfo`, and re-reads CMI before returning a
verified outcome. Missing baseline/readback preservation fields fail closed
instead of becoming synthetic defaults. Authentication before mutation may
renew once; no mutation is replayed after an authentication failure.
DurationRead remains independent from this write lifecycle and never starts a
missing CMI session.

`ResourceExecution` is separately advertised for visible SCOs. It freezes a
fixed score or deterministically selects one value from the configured bounded
range for the attempt, rebinds the exact Course/SCO through fresh detail, then
uses the donor-audited start → setscoinfo → save sequence. A strict fresh CMI
read must show `completed`, progress `1` and the exact selected score. No
post-mutation failure is automatically replayed. `ExecutionVerify` reuses the
frozen runtime settings, performs another full TaskDetail identity rebind and
calls only the non-mutating fresh CMI read. It proves the exact selected tuple,
not generic Completed state, so ambiguous results and crash recovery never
repeat start/setscoinfo/save.

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
