# UAI research sources

Audit date: 2026-08-09. Revisions are frozen before implementation; moving
default branches are not evidence for this checkpoint.

| Source | Revision | Capability evidence | Use | License |
|---|---|---|---|---|
| [`create-try-now/AutoFinish_UxiaoyuanAI`](https://github.com/create-try-now/AutoFinish_UxiaoyuanAI) | `bef0d29155cef727e05ba6b72336ee212c94fe84` | Current Password/JWT, user info, Course list/detail, nested tree, per-Unit completion and direct submit | Reference | Apache-2.0 |
| [`Duster-Cule/UnipusHelperPro`](https://github.com/Duster-Cule/UnipusHelperPro) | `590b4a58fe175240fe9a08fdd69948effcf4f193` | Independent Course/task progress and duration reads; direct-submit limitation | Reference | MIT |
| [`uxudjs/UnipusAIAutoPlayer`](https://github.com/uxudjs/UnipusAIAutoPlayer) | `cc6bdc86a13e7c80a54dff50819607a488ed952e` | Current Unit/Section/Micro DOM plus page-residence duration behavior | Reference | GPL-3.0 |

The Apache backend donor is the primary current HTTP reference. The MIT donor
is an independent route/schema cross-check and proves that completion,
progress and duration are separate observations. The GPL userscript is used
only to understand browser lifecycle behavior; no implementation code is
copied. Asterism's native authentication boundary and read-only parsers remain
offline-covered; no donor implementation code is copied.
