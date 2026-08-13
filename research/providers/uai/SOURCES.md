# UAI research sources

Audit dates: 2026-08-09 and 2026-08-11. Revisions are frozen before implementation; moving
default branches are not evidence for this checkpoint.

| Source | Revision | Capability evidence | Use | License |
|---|---|---|---|---|
| [`create-try-now/AutoFinish_UxiaoyuanAI`](https://github.com/create-try-now/AutoFinish_UxiaoyuanAI) | `bef0d29155cef727e05ba6b72336ee212c94fe84` | Current Password/JWT, Course/progress, encrypted content/answer, typed objective/subjective/compound execution, discussion, exit-ticket, oral-empty, upload and external AI behavior | Reference | Apache-2.0 |
| [`Duster-Cule/UnipusHelperPro`](https://github.com/Duster-Cule/UnipusHelperPro) | `590b4a58fe175240fe9a08fdd69948effcf4f193` | Independent Course/task progress and duration reads; encrypted answer, ordered single/multi-module submit builders and fresh user-module verification routes | Reference | MIT |
| [`uxudjs/UnipusAIAutoPlayer`](https://github.com/uxudjs/UnipusAIAutoPlayer) | `cc6bdc86a13e7c80a54dff50819607a488ed952e` | Current Unit/Section/Micro DOM and iframe discovery, Tab/Task interaction, popup handling, page-residence distribution and optional video playback/keepalive | Reference | GPL-3.0 |
| [`Zzj-klwgxdz/UnipusAI`](https://github.com/Zzj-klwgxdz/UnipusAI) | `40ead69c7dabf7a2f3a215ff69f3feba73a736f6` | Current Rust progress-leaf `tab_type`, text/video mark-seen, generic ordered child answer body, content-derived judge types, external LLM and media transcription | Reference | GPL-3.0 |

The Apache backend donor is the primary current HTTP reference. The MIT donor
is an independent route/schema cross-check and proves that completion,
progress and duration are separate observations. Its response contract also
explicitly identifies the study-record duration values as seconds and the
route caller binds query `id` to the numeric CourseResource ID. The GPL
userscript is used only to understand browser lifecycle behavior; no
implementation code is copied. Its frozen `5.2.14` source specifically
evidences the two browser origins, legacy/Ant/u3menu directory discovery,
Micro→Tab→Task traversal, bounded popup retries, pause/restart timing, optional
Video.js playback and the SCAN/CLICK/result iframe protocol. Its wildcard
`postMessage` trust is not copied: Asterism requires origin, frame, session and
account/Task binding. Asterism's native authentication, parser and mutation
boundaries remain offline-covered. The two backend donors independently
corroborate the
annotator-token contract across content, progress and submission routes;
Asterism reimplements that bounded protocol without copying donor
implementation code.

The current Rust donor additionally documents that a normal authenticated
`ucontent` request carries both raw `Authorization` and `u-openid`. Capture
recipe v1 maps those two same-snapshot headers into one strict
`ProviderCompositeSession` JSON output under `AssistedSession`, with only the
two browser-donor origins allowlisted. This is sufficient Provider-side recipe
evidence; executing the declarative recipe remains shared Capture-helper work.

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
rows in the same order. The current Rust donor also solves every child inside a
choice module independently, which is the evidence for Asterism's Composite
Question mapping and per-child option/answer binding. Their client-authored auxiliary fields differ: the
current donor uses a minimal `1/1` context/answer version pair, whereas the MIT
multi builder uses `1/0` plus fabricated course-answer score maps. Asterism
records the common ordered contract, uses the current minimal multi body with
content-derived per-child judge labels, never copies or fabricates score maps,
and retains Development verification.

The Apache donor audit additionally confirms the objective label set
`material-banked-cloze`, `basic-scoop-content`,
`basic-scoop-content-dropdown`, `fillblank-scoop-dropdown`, `sequence`,
`translation` and `revise-mistake`, plus discussion topic/reply APIs,
exit-ticket/oral empty submission and CMS-token/object-store upload. These are
implementation scope. Where the shared Core cannot yet express a reply draft,
artifact handle or external resolver, that is a Core Gap rather than a Provider
policy exclusion.
