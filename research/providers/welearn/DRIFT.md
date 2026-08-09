# WELearn drift register

| Risk | Static evidence | Required response |
|---|---|---|
| OIDC authorize/return shape changes | 2025 and 2026 donors construct/extract `rturl` differently | Parse redirects structurally, allowlist hosts and fixture each observed variant |
| OIDC introduces another trusted host or redirect count grows | The audited flow currently stays on two hosts and completes within a short chain | Fail closed at the host/redirect bound; expand only from sanitized current evidence |
| SSO Cookie domain/path layout changes | Current native boundary accepts only three audited domains and applies path scoping | Treat rejected scope or missing authenticated Course read as protocol drift, never relax from an error body alone |
| Course path alternates between `/student` and `/2019/student` | Donors mix both paths | Treat one as current only after live comparison; do not scatter hard-coded paths |
| `courseunits` accepts GET and POST in different donors | Both forms appear in audited revisions | Select one after live validation and retain the other only as a classified fallback |
| Completion labels are localized or schema-dependent | `scoLeaves` donors inspect text fields directly | Keep raw sanitized label and map unknown values to `Unknown` |
| `getscoinfo_v7` outer or nested shape changes | Audited response stores JSON text inside the outer `comment` string and may omit `cmi` before first start | Keep both layers bounded, reject ambiguous shapes, and never start a SCO to repair a read |
| CMI progress grammar changes | Current donor reads decimal `progress_measure` in the `0..1` range | Accept only the bounded audited decimal grammar; add sanitized evidence before widening it |
| CMI time units/format drift | Donors forward opaque `session_time`/`total_time` values | Parse and report only after real response fixtures establish units and grammar |
| Heartbeat semantics drift | Donors send the long action name at fixed intervals | Make interval a Master-controlled Provider default with per-task override, bounded by Provider safety limits |
| Completion can be forged independently of duration | Donor has direct completed/progress/score payloads | Exclude that path; read, preserve, mutate narrowly and verify with fresh CMI |
| Session challenge appears after password failures | Current donor reports captcha/SMS branches | Stop retries and classify `HumanRequired`; Capture remains deferred for batch one |
| Expired sessions return either redirects or `200` login HTML | Donors and web stacks vary in expiry presentation | Detect only structural login pages, renew a native Composite once, restart the complete read and never retry other error kinds as authentication |

## Live-validation gate

Before `VerificationLevel` can advance:

1. validate password login, Cookie persistence, expiry and renewal;
2. compare Course/Unit/SCO inventory with the visible account;
3. record completion, progress, session time and total time for not-started,
   in-progress and completed SCOs;
4. exercise start → keep → finalize on an allowed non-assessment resource;
5. re-read CMI and Course/SCO facts after mutation;
6. record one challenge, auth expiry or protocol-error branch;
7. save only sanitized fixtures and a dated verification record.
