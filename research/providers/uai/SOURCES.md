# UAI research sources

Audit dates: 2026-08-09, 2026-08-11 and 2026-08-13. Recorded revisions are
reproducible implementation snapshots; each checkpoint also refreshes default
branches, tags/releases and relevant protocol commits for incremental audit.

| Source | Revision | Capability evidence | Use | License |
|---|---|---|---|---|
| [`create-try-now/AutoFinish_UxiaoyuanAI`](https://github.com/create-try-now/AutoFinish_UxiaoyuanAI) | `bef0d29155cef727e05ba6b72336ee212c94fe84` | Current Password/JWT, Course/progress, encrypted content/answer, typed objective/subjective/compound execution, discussion, exit-ticket, oral-empty, upload and external AI behavior | Reference | Apache-2.0 |
| [`Duster-Cule/UnipusHelperPro`](https://github.com/Duster-Cule/UnipusHelperPro) | `590b4a58fe175240fe9a08fdd69948effcf4f193` | Independent Course/task progress and duration reads; encrypted answer, ordered single/multi-module submit builders and fresh user-module verification routes | Reference | MIT |
| [`uxudjs/UnipusAIAutoPlayer`](https://github.com/uxudjs/UnipusAIAutoPlayer) | `cc6bdc86a13e7c80a54dff50819607a488ed952e` | Current Unit/Section/Micro DOM and iframe discovery, Tab/Task interaction, popup handling, page-residence distribution and optional video playback/keepalive | Reference | GPL-3.0 |
| [`Zzj-klwgxdz/UnipusAI`](https://github.com/Zzj-klwgxdz/UnipusAI) | `40ead69c7dabf7a2f3a215ff69f3feba73a736f6` | Current Rust progress-leaf `tab_type`, required/minimum-score/time-window/statistic strategy, text/video mark-seen, generic ordered child answer body, content-derived judge types, external LLM and media transcription | Reference | GPL-3.0 |

The Apache backend donor is the primary current HTTP reference. The MIT donor
is an independent route/schema cross-check and proves that completion,
progress and duration are separate observations. Its response contract also
explicitly identifies the study-record duration values as seconds and the
route caller binds query `id` to the numeric CourseResource ID. Its task runner
also corroborates the strict nonzero start/end availability check used before
interaction. The GPL
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

The same MIT donor treats
`studyRecord/totalAndUnitSituation?id={CourseResourceId}&appUserId={appUserId}`
as a separate Course/Unit read. The response carries Course total and Unit
finish progress, seconds, score and required state; Asterism therefore keeps a
dedicated Provider-private snapshot instead of collapsing those aggregates
into Task progress. Shared exposure remains a Core contract item.

The MIT donor separately calls `courseStudyStrategy/detail` with the fresh
CourseResource strategy ID, then uses the returned per-Unit `requiredTask`
lists as its execution selection set. The same response evidences Unit and
Course windows, pass-score/score-type and unlock/scoring modes. Asterism ports
that route as an independent Provider-private policy snapshot; it does not
erase the current Rust donor's independently evidenced progress-leaf required
flag.

On 2026-08-13, all four UAI source checkouts were fetched with tags and compared
against their default remote branches. Apache `bef0d29155ce`, MIT
`590b4a58fe17`, AutoPlayer `cc6bdc86a13e` and current Rust
`40ead69c7dabf` were each at zero divergence; no new default-branch or tag delta
required an incremental port at this checkpoint. These revisions remain
reproducible audit snapshots, not permanent update ceilings.
The same four default branches and tag tips were fetched and confirmed at zero
delta again after the independent Course-progress checkpoint.

The rendered entry-route audit also uses two corroborating behaviors without
copying implementation code: the current Rust donor sends
`https://ucontent.unipus.cn/_explorationpc_default/pc.html` as the browser
Referer, and Duster-Cule/UnipusHelper issue `#26` records a real 2025 page with
that path and `cid={CourseResourceId}` before optional UI parameters and a
courseware hash. Asterism retains only that minimal stable HTTPS route; the
fresh hierarchy protocol performs the exact Task selection after navigation.

The current Rust donor additionally documents that a normal authenticated
`ucontent` request carries raw `Authorization`, `u-openid` and optionally
`u-school`. Capture recipe v4 maps those same-snapshot headers plus the exact
`ucontent` Cookie into a strict `ProviderCompositeSession` JSON output and a
separately purpose-bound `ProviderCookie` under `AssistedSession`. Its ordered
composite alternatives retain `u-school` when observed and remain complete
when the optional header is absent; navigation stays limited to the two
browser-donor origins and request reads to `ucontent`.
This is sufficient Provider-side recipe evidence; executing the declarative
recipe remains shared Capture-helper work.

The 2026-08-10 Rust donor is the newest frozen execution reference. Its actual
runner classifies fresh progress leaves with `tab_type=text|video` as mark-seen
resources and sends the exact empty `submitType=2` body. The MIT donor's
single-type builder independently emits that same body. The Apache donor agrees
on the five preset `base` labels but its active generic builder inserts
`instanceId=0` placeholder question rows, so that path is recorded as drift and
is not body evidence. Asterism uses the five labels only to identify scan-time
candidates and requires a fresh exact text/video progress leaf before mutation.
The same current progress model exposes per-leaf `required`, `min_score_pct`,
`start_time`, `end_time` and `statistic_mode_out`; Asterism retains those facts
for Task selection and applies the donor time-window rule to every native
mutation rather than relying on stale tree labels.

The current Rust donor also performs a separate Course-level progress read at
`course_progress/{courseInstanceId}/{openid}/default` before the per-Unit
reads. It takes both the complete Unit-key map and `publish_version` from that
response, then uses the version in later submission judge metadata. Asterism
now ports this route independently: the bounded snapshot must agree with the
fresh tree's complete Unit set, fills a missing tree version, rejects a
conflicting copy and retains the Course-Unit strategy facts separately from
each Group leaf's strategy.

For answer-bearing Groups, the current Rust and MIT donors both serialize one
ordered `quesDatas` row per native module and flatten child completion/judge
rows in the same order. The current Rust donor also solves every child inside a
choice module independently, which is the evidence for Asterism's Composite
Question mapping and per-child option/answer binding. Their client-authored
auxiliary fields differ: the current donor uses a minimal `1/1` context/answer
version pair, whereas the MIT multi builder uses `1/0` plus fabricated
course-answer score maps. Independently, the current donor reads Course
`publish_version` from fresh progress and emits it as each judge's Course
version with `answer=3`; the MIT builder evidences a legacy judge `0/0` pair.
Asterism records the common ordered contract, uses the current minimal multi
body and fresh publish-bound judge versions when supplied, retains the legacy
judge pair only when that Course field is absent, never copies or fabricates
score maps, and retains Development verification.

The MIT donor additionally implements `writing` by reading the standard-answer
analysis as text and submitting it through the ordinary answer-bearing route;
Asterism maps that exact semantic to ShortAnswer. The Apache donor audit
additionally confirms the objective label set
`material-banked-cloze`, `basic-scoop-content`,
`basic-scoop-content-dropdown`, `fillblank-scoop-dropdown`, `sequence`,
`translation` and `revise-mistake`, plus discussion topic/reply APIs,
exit-ticket/oral empty submission and CMS-token/object-store upload. These are
implementation scope. Where the shared Core cannot yet express a reply draft,
artifact handle or external resolver, that is a Core Gap rather than a Provider
policy exclusion.

The MIT donor also treats `video-popup` as an answer-bearing type, reads the
first standard-answer value from every child and selects `submitType=2` because
the exact base remains a study mode. The current Rust donor independently
parses `video-popup` module/child `replyType`, solves its choice/text/banked
children and serializes their exact judge labels. Asterism combines those
compatible facts: content determines the typed answer shape, all answer rows
are retained, and an all-`video-popup` plan uses type 2 rather than the empty
preset body.
