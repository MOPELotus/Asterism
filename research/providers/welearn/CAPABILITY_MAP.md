# WELearn capability map

| Asterism capability | Primary evidence | Use | Current decision |
|---|---|---|---|
| Authentication | Fanyuchang 2026 | Reference | Native Password/OIDC manual redirects, scoped Cookie collection and typed captcha/SMS outcomes are offline/native-boundary covered; live pending |
| Stored session validation | Fanyuchang 2026 | Reference | Core-scoped Cookie resolution, authenticated Course-list validation and atomic Password/Composite renewal are native-boundary covered; live pending |
| CourseInventory | Fanyuchang 2026 + YZBRH | Reference | Native authenticated `authCourse.aspx?action=gmc` read is implemented behind shared NetworkProfile; live pending |
| TaskInventory | Fanyuchang 2026 + YZBRH | Reference | Native Course page → `courseunits` → one `scoLeaves` response per Unit is implemented all-or-nothing; live pending |
| TaskDetail | Fresh Course/Unit/SCO inventory | FromScratch | Re-lists Courses and re-runs one complete Course scan, then requires exactly one matching stable SCO identity; live pending |
| TaskProgressRead | YZBRH | Reference | Implemented through a fresh read-only `getscoinfo_v7` CMI request; completion and progress are parsed independently; live pending |
| DurationRead | YZBRH | Reference | Parser retains bounded raw `session_time` and `total_time` independently; no Core duration or seconds conversion until live unit evidence exists |
| ResourceExecution | Fanyuchang 2026 + YZBRH | Reference | Completion-changing execution remains unimplemented; donor score/completion-forging paths stay excluded |
| DurationReport | YZBRH + Fanyuchang 2026 | Reference | Native fresh-read → optional start → complete preservation baseline → bounded real-time heartbeat → preserve-and-finalize → strict fresh-read lifecycle is offline/native-boundary covered; live pending |
| Submission/assessment | None selected | FromScratch | Out of this inventory slice; donor score-forging paths are not accepted behavior |
| Result verification | Fresh Course/SCO/CMI reads | FromScratch | Duration report requires fresh CMI, unchanged completion/progress/score and a changed raw time observation; live pending |
| BrowserBridge | No current need | FromScratch | First batch defers Capture/browser-dependent work |

## Initial implementation boundary

The first code slice is fixture-only and read-only:

1. parse a bounded Course list into `RemoteCourse`;
2. parse Unit and SCO-leaf responses into `RemoteTask`;
3. keep SCO completion and donor-labelled learning time as separate facts,
   without claiming a time unit before live CMI evidence exists;
4. expose no execution, duration-report, submission or Capture slot.

The crate now provides this parser boundary and synthetic fixtures. It also
implements Password/ImportedCookie orchestration behind injected transport and
stored-session resolver contracts. Metadata remains `Development` and advertises
Authentication, CourseInventory, TaskInventory, TaskDetail and TaskProgressRead. A registry-consistent
development entry can be composed from injected boundaries. A shared-policy,
non-redirecting native Password/OIDC and Course/Task HTTP transports now exist.
Stored credential resolution now validates exact account/reference/purpose,
session kind, expiry and UTF-8 shape before constructing a Cookie session.
Daemon registration is available only through an explicit disabled-by-default
development flag and requires a configured SecretStore. Password/Composite
renewal uses Core compare-and-replace and retries one read operation only after
an Authentication failure. Fresh CMI reads re-resolve the Course route, post
only `getscoinfo_v7`, and never start or mutate a SCO as a fallback. Raw CMI
times are available to the parser but `RemoteProgress.duration_seconds` remains
unset. Fresh TaskDetail re-discovers the selected Course and exact SCO from the
same complete inventory path; disappeared identities return RemoteChanged and
versioned fingerprints satisfy the Core detail boundary. All live validation
remains pending.

The next implemented mutation boundary is `DurationReport` only. Master-owned
runtime settings provide platform defaults plus account/task overrides for the
actual report length and heartbeat interval, while Core-owned concurrency and
scan-interval behaviors stay explicit. The native lifecycle keeps one resolved
Cookie and fresh Course route through baseline read, optional start, bounded
heartbeats and finalization. It preserves completion, progress, score and
success status, never calls `setscoinfo`, and re-reads CMI before returning a
verified outcome. Missing baseline/readback preservation fields fail closed
instead of becoming synthetic defaults. Authentication before mutation may renew once; no mutation is
replayed after an authentication failure. `DurationRead` remains absent because
raw remote time units are still not live-proven, and completion-changing
`ResourceExecution` remains separate and unadvertised.

The shared Engine now evaluates a DurationReport-only execution against the
duration goal instead of requiring the remote Task to become Completed. A
verified Pending, InProgress or Completed readback can therefore finish the
Execution, while every Provider error goes directly to HumanRequired without a
retry. Recovery of an abandoned duration report never calls TaskProgress and
never re-enters the write lifecycle. This closes the offline Core contract gap;
live Provider validation remains pending.
