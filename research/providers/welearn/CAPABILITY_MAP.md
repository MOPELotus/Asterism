# WELearn capability map

| Asterism capability | Primary evidence | Use | Current decision |
|---|---|---|---|
| Authentication | Fanyuchang 2026 | Reference | Native Password → OIDC Cookie session; recognize ImageCaptcha/SmsVerification and stop retries |
| Stored session validation | Fanyuchang 2026 | Reference | Validate with an authenticated read; never infer validity from Cookie presence |
| CourseInventory | Fanyuchang 2026 + YZBRH | Reference | `authCourse.aspx?action=gmc`; parse `clist` conservatively |
| TaskInventory | Fanyuchang 2026 + YZBRH | Reference | Course page context → `courseunits` → `scoLeaves`; preserve Unit/SCO identity |
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

The crate now provides this parser boundary and synthetic fixtures. Its metadata
remains `Development` and advertises no capability, authentication method or
session kind until native boundaries are implemented and registered.
