# UAI research sources

Audit dates: 2026-08-09 and 2026-08-11. Revisions are frozen before implementation; moving
default branches are not evidence for this checkpoint.

| Source | Revision | Capability evidence | Use | License |
|---|---|---|---|---|
| [`create-try-now/AutoFinish_UxiaoyuanAI`](https://github.com/create-try-now/AutoFinish_UxiaoyuanAI) | `bef0d29155cef727e05ba6b72336ee212c94fe84` | Current Password/JWT, user info, Course list/detail, nested tree, encrypted content/answer, per-Unit completion and direct submit | Reference | Apache-2.0 |
| [`Duster-Cule/UnipusHelperPro`](https://github.com/Duster-Cule/UnipusHelperPro) | `590b4a58fe175240fe9a08fdd69948effcf4f193` | Independent Course/task progress and duration reads; encrypted answer, ordered single/multi-module submit builders and fresh user-module verification routes | Reference | MIT |
| [`uxudjs/UnipusAIAutoPlayer`](https://github.com/uxudjs/UnipusAIAutoPlayer) | `cc6bdc86a13e7c80a54dff50819607a488ed952e` | Current Unit/Section/Micro DOM plus page-residence duration behavior | Reference | GPL-3.0 |
| [`Zzj-klwgxdz/UnipusAI`](https://github.com/Zzj-klwgxdz/UnipusAI) | `40ead69c7dabf7a2f3a215ff69f3feba73a736f6` | Current Rust progress-leaf `tab_type`, exact text/video mark-seen body, ordered multi-module answer body and submit route | Reference | GPL-3.0 |

The Apache backend donor is the primary current HTTP reference. The MIT donor
is an independent route/schema cross-check and proves that completion,
progress and duration are separate observations. Its response contract also
explicitly identifies the study-record duration values as seconds and the
route caller binds query `id` to the numeric CourseResource ID. The GPL userscript is used
only to understand browser lifecycle behavior; no implementation code is
copied. Asterism's native authentication boundary and read-only parsers remain
offline-covered. The two backend donors independently corroborate the
annotator-token contract across content, progress and submission routes;
Asterism reimplements that bounded protocol without copying donor
implementation code.

The 2026-08-10 Rust donor is the newest frozen execution reference. Its actual
runner classifies fresh progress leaves with `tab_type=text|video` as mark-seen
resources and sends the exact empty `submitType=2` body. The MIT donor's
single-type builder independently emits that same body. The Apache donor agrees
on the five preset `base` labels but its active generic builder inserts
`instanceId=0` placeholder question rows, so that path is recorded as drift and
is not body evidence. Asterism uses the five labels only to identify scan-time
candidates and requires a fresh exact text/video progress leaf before mutation.

For answer-bearing Groups, the current Rust and MIT donors both serialize one
ordered `quesDatas` row per native module and flatten child completion/judge
rows in the same order. Their client-authored auxiliary fields differ: the
current donor uses a minimal `1/1` context/answer version pair, whereas the MIT
multi builder uses `1/0` plus fabricated course-answer score maps. Asterism
records the common ordered contract, uses the current minimal multi body with
content-derived per-child judge labels, never copies or fabricates score maps,
and retains Development verification.
