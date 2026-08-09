# WELearn capability map

| Asterism capability | Primary evidence | Use | Current decision |
|---|---|---|---|
| Authentication | Fanyuchang 2026 | Reference | Native Password/OIDC manual redirects, scoped Cookie collection and typed captcha/SMS outcomes are offline/native-boundary covered; live pending |
| Stored session validation | Fanyuchang 2026 | Reference | Core-scoped Cookie resolution plus native Course-list validation exist; renewal and live validation remain pending, and Cookie presence alone is never accepted |
| CourseInventory | Fanyuchang 2026 + YZBRH | Reference | Native authenticated `authCourse.aspx?action=gmc` read is implemented behind shared NetworkProfile; live pending |
| TaskInventory | Fanyuchang 2026 + YZBRH | Reference | Native Course page → `courseunits` → one `scoLeaves` response per Unit is implemented all-or-nothing; live pending |
| TaskProgressRead | YZBRH | Reference | Fresh CMI and SCO facts; completion and progress are distinct |
| DurationRead | YZBRH | Reference | Read `session_time` and `total_time` independently from completion |
| ResourceExecution | Fanyuchang 2026 + YZBRH | Reference | Start/keep/finalize lifecycle; not implemented until readback is modeled |
| DurationReport | YZBRH | Reference | Heartbeat uses explicit session/total time; no blind completion mutation |
| Submission/assessment | None selected | FromScratch | Out of this inventory slice; donor score-forging paths are not accepted behavior |
| Result verification | Fresh Course/SCO/CMI reads | FromScratch | Require post-mutation readback; HTTP success is insufficient |
| BrowserBridge | No current need | FromScratch | First batch defers Capture/browser-dependent work |

## Initial implementation boundary

The first code slice is fixture-only and read-only:

1. parse a bounded Course list into `RemoteCourse`;
2. parse Unit and SCO-leaf responses into `RemoteTask`;
3. keep SCO completion and donor-labelled learning time as separate facts,
   without claiming a time unit before live CMI evidence exists;
4. expose no authentication, network transport, execution or submission slot.

The crate now provides this parser boundary and synthetic fixtures. It also
implements Password/ImportedCookie orchestration behind injected transport and
stored-session resolver contracts. Metadata remains `Development` and advertises
Authentication, CourseInventory and TaskInventory. A registry-consistent
development entry can be composed from injected boundaries. A shared-policy,
non-redirecting native Password/OIDC and Course/Task HTTP transports now exist.
Stored credential resolution now validates exact account/reference/purpose,
session kind, expiry and UTF-8 shape before constructing a Cookie session.
Daemon registration is available only through an explicit disabled-by-default
development flag and requires a configured SecretStore. Renewal and all live
validation remain pending.
