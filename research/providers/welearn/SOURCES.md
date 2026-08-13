# WELearn upstream sources

Audit date: 2026-08-13

Default-branch heads were rechecked on 2026-08-13 and still match all three
pinned revisions below; no new donor capability surface was introduced.
The same date's read-only public SSO audit observed the current static bundle,
the host/path of unauthenticated external-login redirects, and the exact
Course-list login-script response produced with anonymous prelogin Cookies; no
account, credential, challenge answer or callback was used.

This is a static source audit. No donor or Asterism implementation was tested
against a real WELearn account, so every live-compatibility claim remains
pending.

| Source | Revision | Updated | Use | Audited implementation surface | Live status |
|---|---|---|---|---|---|
| [`Fanyuchang2026/welearn-helper`](https://github.com/Fanyuchang2026/welearn-helper) | `afa87fb7c86d6b3fdaa63ed08b4a02b4b112e2a2` | 2026-06-09 | Reference | Current SSO/OIDC password flow and interactive captcha/SMS/third-party browser behavior, Course/Unit/SCO discovery including deliberately unfiltered hidden/already-completed SCO execution, clamped-Gaussian selected score followed by an unconditional second `crate=100` save, duration heartbeat and concurrency behavior | Offline/native/Capture response-readiness helper boundary covered; OAuth callback and live learning validation pending |
| [`YZBRH/Welearn_helper`](https://github.com/YZBRH/Welearn_helper) | `bd160e91d0452b8bf483087fbdd3bdd58d855e13` | 2025-12-25 | Reference | Redirect handling, TLS-client behavior, direct `setscoinfo`/save accuracy preset, CMI read, heartbeat and final save | Offline/native boundary covered; live pending |
| [`1q2w-c/Auto_WeLearn`](https://github.com/1q2w-c/Auto_WeLearn) | `85918caaccd93b73b1e41fe537b4e9a11377b759` | 2025-12-14 | Historical | Modular API boundary, multi-account management, fixed/random score UX, selected-Unit/all-task batch execution, aggregate duration-budget distribution and explicit 1–100 worker setting, concurrent completion and duration route cross-check | Protocol/settings cross-check covered; shared Unit/batch orchestration gap recorded |

## Upstream refresh

The 2026-08-13 refresh fetched every default branch and tag before the next
implementation checkpoint and again before the Unit/batch capability-family
audit. Default heads remain `afa87fb7c86d` (Fanyuchang
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

## Source selection

- Use the June 2026 donor to identify current route/response facts and the
  complete currently evidenced capability surface; do not prune its direct
  completion/progress/score behavior on product-policy grounds.
- Use the YZBRH donor to cross-check the full duration lifecycle: read CMI,
  start if absent, heartbeat while retaining existing values, then finalize.
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
and the exercise donors' set-then-save mutation family are separately exposed;
well-formed negative receipts do not suppress later evidenced writes. They
also preserve distinct start requests: current Fanyuchang uses full route
identity with a simple Referer, while YZBRH/Auto use minimal SCO identity and a
task Referer for exercise completion. The current route appends `?uid=...` to
`/Ajax/SCO.aspx`; both historical donors use the plain endpoint. These are
explicit profiles rather than one merged superset request. They
also detect authentication challenges but contain no reliable automated solver
or task BrowserBridge implementation to port. Asterism provides immutable
Capture v4/v5 alternatives for AssistedSession and ExternalBrowserOauth Cookies
obtained in browser context and validates both natively. The external method
records the flow family without inventing a callback-to-Cookie exchange or a
QQ/WeChat/Apple discriminator. Automated challenge behavior remains
evidence-gated, not policy-excluded; later donor/live evidence reopens the map.
