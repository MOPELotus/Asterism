# Cidaren donor and protocol differences

Audit date: 2026-08-14. Each revision below is an immutable evidence point for
this audit, while the upstream tracking target remains renewable. Branch,
tag/release and key-commit movement is logged in
[`UPSTREAM_CHECKS.md`](UPSTREAM_CHECKS.md) before a new snapshot is adopted;
generated release pages and local helper binaries are never silently
substituted for source evidence.

The complete from-scratch comparison across all configs, tags/releases,
issues, examples and call sites is in
[`FULL_UPSTREAM_SWEEP.md`](FULL_UPSTREAM_SWEEP.md).

## Frozen lines

| Evidence line | Frozen revision | Role |
|---|---|---|
| `ularch/Easy_Cidaren` default `master` | `bce9559f536ebbdad791f41ed4e111b30accb05d` | Reopened current public source. Commit `c8de5f4` contains the executable update; `bce9559` documents the reopening and changelog |
| `ularch/Easy_Cidaren` latest published release | tag `1.5.4` / commit `7e29ee43692f4c0807fae8cf7f74a5a674793097` | Historical packaged release published 2025-12-09. Asset `Easy_Cidaren1_5_4.zip`, size `103462537`, SHA-256 `526011a4ccd14cc38887a663d54a5c78c33d8ea3d48e6c424a32128b6d0d8aca` |
| `MOPELotus/Easy_Cidaren` default `master` | `a74b4a2c1cdc5d38f568d41ccb11bb4d441ee4f1` | Owner-supplied current crypto/Capture PortSource frozen 2026-05-13 |
| OAuth V2 assisted-bootstrap handoff | archive SHA-256 `5c09f74d5c73df6339ad4daac48f51dff218f10219c9cdf417d9f2aa12384f70` | First-party H5 and authorized device-flow evidence for the current no-MITM OAuth callback and V2 crypto bootstrap |

The published `1.5.4` release predates the reopened public `master`; it is a
release provenance checkpoint, not the latest source implementation. The
public and owner-supplied branches diverged after `f2b5c25`, so neither branch
is allowed to erase capabilities evidenced only by the other. The OAuth
handoff is a third, independent current protocol line and is not inferred from
either Python donor.

## Protocol differences

| Protocol area | Reopened `ularch@bce9559` | `MOPELotus@a74b4a2` | OAuth V2 handoff | Asterism mapping |
|---|---|---|---|---|
| Existing token validation | Opaque `UserToken`; mobile WeChat User-Agent; `Abc=md5(UA)`; read `Authorization-V=md5("0")`; `Student/Main` | Same header family and account read | Fresh OAuth result is validated with current desktop UA, `Authorization-v: 00`, derived `Abc`, version `2.7.0.260715_01` | Both contexts are explicit; native OAuth readback never falls through to older Capture headers |
| Local Capture | Built-in GUI wrapper saves/restores the Windows system proxy, clears and polls `token.txt` every 500 ms, and runs `mitmdump -s addon.py` on fixed `127.0.0.1:8888`; response hook records the first `UserToken`; bundled Fiddler files are not invoked | Injected same-origin storage probe plus request-header observation; requires token and `CDR_LOGIN_INFO`, optionally records `CDR_USER_SESSION`; writes Composite crypto context | No Capture/MITM required: user returns a preserved random-marker OAuth callback | Provider validates and advertises Composite-first, token-only-second recipes; Core freezes one recipe version per bootstrap without mixing outputs. The shared runner now executes the strict command and reads only its declared header/storage facts, but its temporary Chromium profile does not inherit the donor's authenticated XWeb state or reproduce the public donor's system-proxy/certificate lifecycle; obtaining and validating that real browser context remains a shared environment/live gate |
| OAuth acquisition | No native OAuth exchange in reopened Python source | No native OAuth exchange; obtains browser state through Capture | Exact WeChat OAuth URL with independent CSPRNG state/marker, marker not `"2"`, manual callback return | Structured External OAuth challenge, hash-only durable binding, strict callback parser and one-shot exchange implemented |
| Login exchange crypto | None | None | P-256 SPKI, signed V2 POST, ECDH, HKDF-SHA256 `vcg-auth-aes`, AES-256-GCM with exact AAD | Native Rust implementation plus same-context account readback and atomic Composite replacement |
| Current encrypted response | No `jv=99` implementation | `CDR_LOGIN_INFO` prefixes `h`/`a`; HKDF `vcg-exam-aes:{ucd}`; AES-GCM with response AAD | V2 exchange yields the same account-bound material | Existing bounded/zeroizing `jv=99` implementation is retained; public reopening cannot roll it back |
| Legacy `jv=2_*` | Exact fixed-index tables | Exact fixed-index tables | Not relevant | Exact declared transforms only |
| Legacy `jv=3_1021` | Sequential removals then inverse five-chunk order `[1,3,2,0,4]` | Fixed-index removal | Not relevant | Evaluate only these two evidenced candidates; accept a unique/equal JSON result and fail closed on ambiguity |
| Legacy `jv=3_2265` / `3_2277` | Sequential removals then inverse five-chunk order `[3,1,0,4,2]` | Absent | Not relevant | Exact public transforms implemented |
| Unknown `jv` handling | Retries GET/POST up to five times | Retries `3_1021` once | Current authenticated framing is exact | Reads may be rediscovered under Core policy; non-idempotent mutations are never replayed merely to obtain another encoding variant |
| Task identity | `release_id` for class tasks; `course_id + list_id` is observable for ordinary units | Same | Not relevant | Fresh rediscovery and stable identities; mutable or `-1` task IDs remain observations only |
| `StartAnswer` task type selector | Reopened UI fixes class request `task_type_int=2` (including selected learning rows) and binds class `release_id`; decoded payload echoes row type 1/2 | Same route family and response echo are retained by the owner line, but no separate current UI call site was added to the audit snapshot | Not relevant | Keep request selector (`Class=2`, `Study=3`) separate from response-row identity (`Class learning=1`, `Class test=2`, `Study=3`); reject a mismatched echo after fresh binding |
| Request/signing versions | Same historical `231204`/`240122` families and MD5 suffix | Same | Current OAuth version `2.7.0.260715_01` | Route-specific versions/sign order are frozen independently |

## Capability differences

| Capability | Reopened public source | Owner-supplied source | OAuth handoff | Current Asterism status |
|---|---|---|---|---|
| Manual/imported token | Yes | Yes | Produces a new token | Implemented |
| Token-only local Capture | Yes; helper terminates or kills its child and restores the prior proxy on success, stop and error | Header observation exists but completion requires crypto | No | Provider validation/resolution, ordered recipe advertisement and strict shared command dispatch/result construction are implemented. Live acquisition still needs either an authenticated isolated Chromium document or a separate shared system-proxy/XWeb bridge; Provider code cannot manufacture that external state |
| Composite Capture / `jv=99` context | No | Yes | Native exchange produces equivalent account-bound material | Implemented and retained |
| No-MITM assisted WeChat OAuth | No | No | Yes; Windows desktop WeChat and Android callback behavior evidenced | Provider and durable Core flow implemented; live product-path validation pending |
| Course and task inventory | Class pages plus selected-Course study units | Same | No | Implemented with bounded complete scans |
| Task detail/progress/duration | Task rows and Info reads | Same | No | Implemented through fresh identity rebinding |
| Post-run task score | Calls `ClassTask/Info` or `StudyTask/Info`, trying `score/task_score/grade`; its dedicated requests session accidentally omits `UserToken` | No dedicated caller; inventory retains score | No | Exact authenticated Native HTTP route implemented after fresh rebinding; aliases are conflict-checked and mapped to Core thousandth-points, with fresh-list fallback for absent score or ambiguous study `task_id=-1` |
| Word inventory/evidence | `StudyTask/Info`, Course page, `StudyWordInfo`, `SearchWord` | Same, with encrypted lookup fix | Supplies crypto needed by current responses | Implemented |
| Question and answer strategy | Single, matching, reading and text families; donor fallback behavior; current payload completed/total counters displayed as remote progress | Same plus current encrypted response support | Supplies crypto/login context only | Implemented privately with immutable Draft-safe answer evidence and bounded remote progress kept separate from local attempt position |
| `SubmitChoseWord` | Yes; issues 48/49 add one exact incomplete-child rejection envelope | Yes | No | Implemented one-shot transport/state edge plus Provider-private definite `RequiredChildrenPending` classification and `cidaren.question-blocked-step.v1` recovery artifact bound to Task/operation/position/dynamic allocation/request/response/time; fresh recovery remains FailedClosed and shared durable blocked outcome remains a Core Gap |
| `StartAnswer` | Yes | Yes | No | Implemented as a non-idempotent attempt start and registered through the public QuestionInventory pre-Question adapter; durable Engine/API orchestration now persists and resumes the one-shot flow |
| `VerifyAnswer` | Yes | Yes | No | Registered session-aware execution issues one relation at a time and rotates the same Question artifact after each accepted topic code |
| `SubmitAnswerAndSave` | Yes | Yes | No | Registered one-shot advance maps intervening reading/selection phases to pre-Question continuation and a real next Question to atomic-transition material |
| `SkipAnswer` | Yes | Yes | No | Registered distinct one-shot skip consumes only explicit persistent `NormalizedAnswer::Skip`; it never treats Unknown or an empty answer as permission |
| Fresh submission verification | Reads task completion/score after run but has no answer history | Fresh task row available; no answer history | No | Fresh Task completion/progress/score implemented; per-Question result remains honestly Unverified |
| Executable BrowserBridge | Donor operates in WeChat/browser context | Capture injects into authenticated H5 | Authorization URL can be rendered/copied/QR-displayed | Provider has a typed, recipe-versioned CaptureSnapshot command/result with exact origin/frame/Task/sequence binding, `jv=99` validation, encrypted command recovery and paired terminal exchange/credential output. The shared Chromium runner executes that command and Core durably commits the validated terminal result, credential replacement and completed session. The remaining gate is live acquisition inside a newly isolated profile versus the donors' authenticated XWeb/system-proxy contexts, not missing Provider dispatch logic. No donor browser action replaces the native assessment mutations |

UI widgets, logging/export, completion sound, update checks, device-ID display
and the unrelated Gitee access-log probe are executable application features
but not Cidaren platform protocol capabilities. The underlying
`topic_done_num/topic_total` response facts used by the progress widget are
nonetheless retained by the Provider. The remaining application-only features
stay audited so their absence is not mistaken for an unreviewed donor surface.
