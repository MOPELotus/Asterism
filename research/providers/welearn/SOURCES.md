# WELearn upstream sources

Audit date: 2026-08-09

This is a static source audit. No donor or Asterism implementation was tested
against a real WELearn account, so every live-compatibility claim remains
pending.

| Source | Revision | Updated | Use | Audited implementation surface | Live status |
|---|---|---|---|---|---|
| [`Fanyuchang2026/welearn-helper`](https://github.com/Fanyuchang2026/welearn-helper) | `afa87fb7c86d6b3fdaa63ed08b4a02b4b112e2a2` | 2026-06-09 | Reference | Current SSO/OIDC password flow, Course/Unit/SCO discovery, CMI completion and duration actions | Offline/native boundary covered; live pending |
| [`YZBRH/Welearn_helper`](https://github.com/YZBRH/Welearn_helper) | `bd160e91d0452b8bf483087fbdd3bdd58d855e13` | 2025-12-25 | Reference | Redirect handling, TLS-client behavior, CMI read, heartbeat and final save | Offline/native boundary covered; live pending |
| [`1q2w-c/Auto_WeLearn`](https://github.com/1q2w-c/Auto_WeLearn) | `85918caaccd93b73b1e41fe537b4e9a11377b759` | 2025-12-14 | Historical | Modular API boundary, multi-account management and older SSO/CMI route cross-check | Pending; not current enough for implementation authority |

## Source selection

- Use the June 2026 donor only to identify current route and response-shape facts.
- Use the YZBRH donor to cross-check the full duration lifecycle: read CMI,
  start if absent, heartbeat while retaining existing values, then finalize.
- Use Auto_WeLearn only as a historical architecture and route cross-check.
- Implement all Rust parsing and transport from scratch because none of the
  audited revisions supplies a clear license grant suitable for source reuse.

No donor source is vendored into Asterism by this audit.
