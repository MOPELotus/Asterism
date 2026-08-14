# WELearn upstream sources

Audit date: 2026-08-14

The mandatory next-checkpoint full sweep was completed on 2026-08-14. It
re-read every tracked README, configuration/dependency file, text source,
default branch, tag/release, commit history, issue and example/fixture surface;
it was not limited to changes after the pins. Default-branch heads still match
all three pinned revisions below, and no new remote protocol route was found.
The 2026-08-13 read-only public SSO audit observed the current static bundle,
the host/path of unauthenticated external-login redirects, and the exact
Course-list login-script response produced with anonymous prelogin Cookies; no
account, credential, challenge answer or callback was used.

This is a static source audit. No donor or Asterism implementation was tested
against a real WELearn account, so every live-compatibility claim remains
pending.

| Source | Revision | Updated | Use | Audited implementation surface | Live status |
|---|---|---|---|---|---|
| [`Fanyuchang2026/welearn-helper`](https://github.com/Fanyuchang2026/welearn-helper) | `afa87fb7c86d6b3fdaa63ed08b4a02b4b112e2a2` | 2026-06-09 | Reference | Current SSO/OIDC password flow and interactive captcha/SMS/third-party browser behavior, Course/Unit/SCO discovery including deliberately unfiltered hidden/already-completed SCO execution, clamped-Gaussian selected score followed by an unconditional second `crate=100` save, duration heartbeat and concurrency behavior | Offline/native/Capture response-readiness helper boundary covered; OAuth callback and live learning validation pending |
| [`YZBRH/Welearn_helper`](https://github.com/YZBRH/Welearn_helper) | `bd160e91d0452b8bf483087fbdd3bdd58d855e13` | 2025-12-25 | Reference | Redirect handling, TLS-client behavior, direct `setscoinfo`/save accuracy preset, CMI read, per-SCO concurrent heartbeat and final save | Offline/native boundary covered; live pending |
| [`1q2w-c/Auto_WeLearn`](https://github.com/1q2w-c/Auto_WeLearn) | `85918caaccd93b73b1e41fe537b4e9a11377b759` | 2025-12-14 | Historical | Modular API boundary, multi-account management, fixed/random score UX, selected-Unit/all-task batch execution, aggregate duration-budget distribution and explicit 1–100 worker setting, plus the distinct sequential single-file completion/duration routes | Protocol/settings cross-check covered; shared Unit/batch orchestration gap recorded |

## Mandatory full-sweep ledger

| Re-enumerated surface | Evidence | Classification after comparison |
|---|---|---|
| Password/OIDC, raw Cookie input, captcha/SMS and external-provider browser choices | Fany source, `login_code.txt`, README and [issue #1](https://github.com/Fanyuchang2026/welearn-helper/issues/1) | Password, ImportedCookie and two bounded Capture alternatives are implemented. The issue confirms donor Cookie mode was not rewritten and may fail; Asterism does not inherit that shortcut and requires an authenticated Course read. Captcha/SMS/OAuth callback live validation remains live-only |
| Course → Unit → SCO inventory and one/all/ordered Unit choices | all three source trees; YZBRH [issue #1](https://github.com/YZBRH/Welearn_helper/issues/1) documents the Course-page `uid`/`classid` shape change | Implemented inventory and Provider-private selection semantics; durable parent selection remains a shared Core Gap |
| Fixed/uniform/clamped-Gaussian score, CMI envelope, start/set/save profiles and current dual save | all completion implementations; Fany tag `v4.0.0` versus `master` | Implemented. The older tag wrote fractional/progress variants that current `master` deliberately replaced; they are recorded as superseded drift, not selectable current wire profiles |
| Per-SCO fixed/random seconds; YZBRH preserved-CMI lifecycle; current one-second client counter; Auto 60-second implicit keeps | all duration implementations; YZBRH [issue #4](https://github.com/YZBRH/Welearn_helper/issues/4) | Implemented or encoded behind the existing atomic Core Gap. Issue #4 requests an inter-SCO delay but the maintainer declined it and no implementation exists, so it is not donor capability evidence |
| Auto modular aggregate budget: base 1–300 minutes, signed random range 0–30, one sample for the selected membership, equal-floor allocation | `ui/account_detail.py`, `ui/workers.py` | Provider-private `WellearnAutoDurationBudget` now retains and validates base/range/frozen offset/actual minutes. Exposing and persisting a parent-batch runtime policy plus immutable parent entropy remains a shared Core Gap; per-Task random duration must not substitute for it |
| 1–100 duration worker pool and independent account sessions | Auto modular UI/workers | Provider/account concurrency bounds are implemented. Cross-account scheduling remains Core-owned |
| Add/remove accounts, nickname/status, CSV/TXT import/export, independent account windows and batch account queue | Auto README, account manager, views and batch manager | Cross-cutting product/Core/API/UI surface, not WELearn wire behavior. Main must decide the shared account-bulk contract; donor plaintext password export must not be copied |
| Retry/timeouts, TLS-client packaging and stop/log display behavior | Fany retry loops; Auto 10-second timeouts; YZBRH [issues #5](https://github.com/YZBRH/Welearn_helper/issues/5), [#6](https://github.com/YZBRH/Welearn_helper/issues/6) and [#8](https://github.com/YZBRH/Welearn_helper/issues/8) | Shared network/runtime/packaging concerns. Existing bounded HTTP policy is retained; donor exception swallowing, forced thread termination and dependency packaging are not Provider capabilities |
| Historical task/result enumeration, per-Question submitted/official answers, grading evidence, pass/completion thresholds and retake | every README/config/source/issue/example/fixture in all three repositories | Unsupported due to missing protocol evidence. Donors expose only Course/SCO CMI completion, score and duration; no Question inventory, result page, answer evidence, threshold or retake route exists. WELearn therefore cannot yet feed the owner-level Answer Evidence Corpus or its bootstrap/Strict Completion/Score Improvement state machines |

No repository contains a protocol fixture directory or executable example
beyond the listed application sources. Binary executables were inventoried but
not executed or reverse engineered. No donor script was run during the sweep.

## Upstream refresh

The 2026-08-13 refresh fetched every default branch and tag before the next
implementation checkpoint, again before the Unit/batch capability-family
audit, and once more after batch target-allocation and bounded-input
hardening. Default heads remain `afa87fb7c86d` (Fanyuchang
`master`), `bd160e91d045` (YZBRH `main`) and `85918caaccd9` (Auto_WeLearn
`master`), so no revision delta was available to port. Fanyuchang tag `v4.0.0`
was also fetched; it resolves to older commit `5d1df60cb007` (2026-05-24), not a
newer capability surface than the audited default head, and is the repository's
only GitHub release. YZBRH and Auto_WeLearn expose neither fetched tags nor
GitHub releases. A second `ls-remote`/release check after the endpoint port
returned the same heads; the subsequent duration-wire, browser-auth, exact
batch-semantics, heartbeat-receipt and heartbeat-cadence checks did as well. These
revisions remain reproducible snapshots; future
checkpoints must fetch again rather than treating them as permanent bounds.
The 2026-08-14 post-selection-boundary refresh again found no donor revision,
tag/release or capability delta. Fanyuchang still exposes only `v4.0.0` and its
matching older release; YZBRH and Auto_WeLearn still expose neither tags nor
GitHub releases. A full YZBRH source recheck also confirmed its top-level
`heartbeat()` is display-only: independently created `simulate()` coroutines
own every network keep and final save, so no shared network-heartbeat dispatch
exists.
The post-mutation-baseline checkpoint on 2026-08-14 repeated default-head,
tag and GitHub-release queries after porting YZBRH's explicit uninitialized-CMI
branch. All three revisions and release surfaces remained unchanged, so that
control-flow correction introduced no new pinned revision.
The post-parent-planning checkpoint on 2026-08-14 queried every remote head and
tag again. Fanyuchang `master` remains full revision
`afa87fb7c86d6b3fdaa63ed08b4a02b4b112e2a2`; its sole tag and GitHub release
remain `v4.0.0` at older revision `5d1df60cb0078f70679be2a22f2afbf1ef4fa88a`.
YZBRH `main` remains `bd160e91d0452b8bf483087fbdd3bdd58d855e13`, and
Auto_WeLearn `master` remains `85918caaccd93b73b1e41fe537b4e9a11377b759`;
both still expose no tags or GitHub releases. No revision, release, protocol or
capability delta was available to port at this checkpoint.

## Source selection

- Use the June 2026 donor to identify current route/response facts and the
  complete currently evidenced capability surface; do not prune its direct
  completion/progress/score behavior on product-policy grounds.
- Use the YZBRH donor to cross-check the full duration lifecycle: read CMI,
  treat its exact `学习数据不正确` response as uninitialized, start if
  explicitly marked, heartbeat while retaining existing values, then finalize.
  Its valid no-CMI branch synthesizes empty fields; Asterism instead stops
  before mutation until live evidence can establish safe preservation values.
- Use Auto_WeLearn as a historical architecture, route, configuration and
  multi-account/concurrency cross-check; require a current donor or live
  evidence before treating a historical-only protocol difference as current.
- Implement all Rust parsing and transport from scratch because none of the
  audited revisions supplies a clear license grant suitable for source reuse.

No donor source is vendored into Asterism by this audit.

The pinned repositories contain no Question/Answer inventory or individual
answer-submission protocol. Their “exercise/accuracy” feature is direct CMI
completion/progress/score mutation and is mapped to ResourceExecution. They
also distinguish ordinary zero-time completion CMI from current Fanyuchang's
duration-then-completion CMI carrying accumulated session/total time; both
shapes are independently configurable and fixture-covered. Current
Fanyuchang's plain JSON and the YZBRH/Auto_WeLearn `[INTERACTIONINFO]` envelope
are likewise separate settings. Auto_WeLearn's save-only duration completion
and the exercise donors' set-then-save mutation family are separately encoded;
well-formed negative receipts do not suppress later evidenced writes. They
also preserve distinct start requests: current Fanyuchang uses full route
identity with a simple Referer, while YZBRH/Auto use minimal SCO identity and a
task Referer for exercise completion. The current route appends `?uid=...` to
`/Ajax/SCO.aspx`; both historical donors use the plain endpoint. These are
explicit profiles rather than one merged superset request. Current
Fanyuchang duration and modular Auto duration each reuse one start through the
final completion mutation; Provider batch plans mark that atomic execution
shape, while shared durable authority/recovery remains required before claiming
the combined sequence is executable. They
also detect authentication challenges but contain no reliable automated solver
or task BrowserBridge implementation to port. Asterism provides immutable
Capture v4/v5 alternatives for AssistedSession and ExternalBrowserOauth Cookies
obtained in browser context and validates both natively. The external method
records the flow family without inventing a callback-to-Cookie exchange or a
QQ/WeChat/Apple discriminator. Automated challenge behavior remains
evidence-gated, not policy-excluded; later donor/live evidence reopens the map.
