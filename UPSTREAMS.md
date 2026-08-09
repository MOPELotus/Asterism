# Upstream provenance

Provider development in Asterism is upstream-first. Before implementing a
provider, add every donor and research source here and in
`research/providers/<provider>/`.

| Provider | Source | Revision | Capability | Use | License | Audited | Live validation |
|---|---|---|---|---|---|---|---|
| `chaoxing` | [`Samueli924/chaoxing`](https://github.com/Samueli924/chaoxing) | `dee643fd0a8e` | Auth, Session, Course, Chapter, Resource, Chapter Work, Work submission shape | Reference | GPL-3.0 | 2026-08-09 | Auth, Course, Chapter and bounded chapter-resource inventory have offline/native-boundary coverage; Document/Read and signed interval-based Video execution plus fresh-card verification are offline-covered; independent Work has a value-free submission preview; remote Work submit/verify, Live/Chapter Work execution and all live validation remain pending |
| `chaoxing` | [`surinrasu/CxKitty`](https://github.com/surinrasu/CxKitty) | `1589eac9c07c` | Auth, Course, Chapter, Exam, Question, Submission, Errors | Reference | GPL-3.0 | 2026-08-09 | Exam list shape and state vocabulary have offline regression coverage; 2024 protocol still needs live verification |
| `chaoxing` | [`iwillwill-ALLWILL/chaoxing-agent-skill`](https://github.com/iwillwill-ALLWILL/chaoxing-agent-skill) | `f72619a0b369` | Independent Work/Exam inventory, status, verification | PortSource | MIT | 2026-08-09 | Inventory and bounded Work status recheck have offline regression coverage; live pending |
| `chaoxing` | [`ocsjs/ocsjs`](https://github.com/ocsjs/ocsjs) | `890686a5e54f` | Current Work/Exam routes and Browser behavior | Reference | MIT | 2026-08-09 | Pending |
| `chaoxing` | [`LangHY/chaoxing-exam`](https://github.com/LangHY/chaoxing-exam) | `14e1dfd9cf11` | Chapter-test navigation, DOM and verification | Reference | No license found | 2026-08-09 | Donor-reported only; Asterism pending |
| `chaoxing` | [`CodFrm/cxmooc-tools`](https://github.com/CodFrm/cxmooc-tools) | `2b81f7b55a68` | Historical Work/Exam Browser routes | Historical | MIT | 2026-08-09 | Not applicable |
| `welearn` | [`Fanyuchang2026/welearn-helper`](https://github.com/Fanyuchang2026/welearn-helper) | `afa87fb7c86d` | 2026 SSO/OIDC, Course/Unit/SCO inventory and CMI lifecycle | Reference | No license found; decompiled/derived lineage | 2026-08-09 | Password/OIDC, scoped Cookie, Core-bound stored-session resolution and renewal, bounded Course/Unit/SCO/CMI reads and disabled-by-default daemon composition have offline/native-boundary coverage; all live validation pending |
| `welearn` | [`YZBRH/Welearn_helper`](https://github.com/YZBRH/Welearn_helper) | `bd160e91d045` | SSO redirects, Course/Unit/SCO inventory, CMI read/heartbeat/finalize | Reference | No license found; README attributes an earlier GPL-3.0 project without supplying license text | 2026-08-09 | Exact `getscoinfo_v7` request and nested CMI read shape have offline parser/native-boundary coverage; heartbeat/finalize and all live validation remain pending |
| `welearn` | [`1q2w-c/Auto_WeLearn`](https://github.com/1q2w-c/Auto_WeLearn) | `85918caaccd9` | Modular API and multi-account architecture; older SSO/CMI behavior | Historical | No license found; README badge is not a license grant | 2026-08-09 | Historical cross-check only |

`Use` must be one of `PortSource`, `Reference`, `Historical`, or `FromScratch`.
