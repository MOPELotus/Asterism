# WELearn drift register

| Risk | Static evidence | Required response |
|---|---|---|
| OIDC authorize/return shape changes | 2025 and 2026 donors construct/extract `rturl` differently | Parse redirects structurally, allowlist hosts and fixture each observed variant |
| OIDC introduces another trusted host or redirect count grows | The audited flow currently stays on two hosts and completes within a short chain | Fail closed at the host/redirect bound; expand only from sanitized current evidence |
| SSO Cookie domain/path layout changes | Current native boundary accepts only three audited domains and applies path scoping | Treat rejected scope or missing authenticated Course read as protocol drift, never relax from an error body alone |
| Course path alternates between `/student` and `/2019/student` | Donors mix both paths | Treat one as current only after live comparison; do not scatter hard-coded paths |
| `courseunits` accepts GET and POST in different donors | Both forms appear in audited revisions | Select one after live validation and retain the other only as a classified fallback |
| Completion labels are localized or schema-dependent | `scoLeaves` donors inspect text fields directly | Keep raw sanitized label and map unknown values to `Unknown` |
| SCO module label is mistaken for assessment nature | Current inventory exposes Resource-like leaves but no formal/routine classification field | Keep assessment class `Unknown`; classify only from a separately audited explicit fact |
| `getscoinfo_v7` outer or nested shape changes | Audited response stores JSON text inside the outer `comment` string and may omit `cmi` before first start | Keep both layers bounded, reject ambiguous shapes, and never start a SCO to repair a read |
| Post-start or verification CMI omits preservation fields | Duration finalization requires explicit completion/progress/score/success and time observations | Stop before heartbeat/finalization when the baseline is incomplete; reject incomplete readback instead of comparing synthesized defaults |
| CMI progress grammar changes | Current donor reads decimal `progress_measure` in the `0..1` range | Accept only the bounded audited decimal grammar; add sanitized evidence before widening it |
| CMI time units/format drift | Current donors advance integer `session_time`/`total_time` against real seconds, but live variants may differ | DurationRead accepts only bounded canonical integer seconds and fails closed on every other grammar; widen only from sanitized evidence |
| Heartbeat semantics drift | Donors send the long action name at fixed intervals | Make interval a Master-controlled Provider default with per-task override, bounded by Provider safety limits |
| Start/keep/save result grammar changes | Audited donors treat start/save `ret=0` and keep `ret=0/1` as accepted | Fail closed on missing, string or unknown result values; widen only from sanitized current evidence |
| Session expires or a network result becomes uncertain after a duration mutation | Replaying start/keep/save could double-report time | Provider never renews-and-replays a mutation; Engine sends every only-DurationReport error or abandoned recovery directly to HumanRequired without retrying or consulting completion progress |
| Completion/progress/score can be changed independently of duration | All pinned donors implement direct `setscoinfo` and/or save payloads | Keep it as independent ResourceExecution; fresh-rebind, freeze selected score, never replay writes, and require exact fresh CMI verification |
| Recovery accidentally reduces the frozen goal to generic Completed | Score is Provider-specific and absent from generic `RemoteProgress` | Use goal-bound `TaskExecutionCapability::verify_execution` with the frozen settings; rebind Task identity and fresh-read exact completion/progress/score without entering any mutation path |
| Session challenge appears after password failures | Current donor reports captcha/SMS branches but supplies no automated solver protocol | Stop retries and classify typed `HumanRequired`; Capture recipe v1 can accept and validate a browser-obtained AssistedSession Cookie, while automated challenge interaction requires sanitized live or newly audited evidence |
| Expired sessions return either redirects or `200` login HTML | Donors and web stacks vary in expiry presentation | Detect only structural login pages, renew a native Composite once, restart the complete read and never retry other error kinds as authentication |

## Live-validation gate

Before `VerificationLevel` can advance:

1. validate password login, Cookie persistence, expiry and renewal;
2. compare Course/Unit/SCO inventory with the visible account;
3. record completion, progress, session time and total time for not-started,
   in-progress and completed SCOs;
4. exercise preservation-mode start → keep → finalize on an authorized resource;
5. exercise direct start → setscoinfo → save with one explicit score setting;
6. re-read CMI and Course/SCO facts after each mutation and confirm the exact
   completion/progress/score or preservation predicate;
7. record one challenge, auth expiry or protocol-error branch and capture its
   sanitized browser/runtime requirements if present;
8. save only sanitized fixtures and a dated verification record.
