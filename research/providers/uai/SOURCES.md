# UAI research sources

Audit dates: 2026-08-09, 2026-08-11, 2026-08-13, 2026-08-14, 2026-08-15 and
2026-08-16. Recorded revisions are
reproducible implementation snapshots; each checkpoint also refreshes default
branches, tags/releases and relevant protocol commits for incremental audit.

| Source | Revision | Capability evidence | Use | License |
|---|---|---|---|---|
| [`create-try-now/AutoFinish_UxiaoyuanAI`](https://github.com/create-try-now/AutoFinish_UxiaoyuanAI) | `bef0d29155cef727e05ba6b72336ee212c94fe84` | Current Password/JWT, Course/progress, encrypted content/answer, typed objective/subjective/compound execution, discussion, exit-ticket, oral-empty, upload and external AI behavior | Reference | Apache-2.0 |
| [`Duster-Cule/UnipusHelperPro`](https://github.com/Duster-Cule/UnipusHelperPro) | `590b4a58fe175240fe9a08fdd69948effcf4f193` | Independent Course/task progress and duration reads; encrypted answer, ordered single/multi-module submit builders and fresh user-module verification routes | Reference | MIT |
| [`uxudjs/UnipusAIAutoPlayer`](https://github.com/uxudjs/UnipusAIAutoPlayer) | `cc6bdc86a13e7c80a54dff50819607a488ed952e` | Current Unit/Section/Micro DOM and iframe discovery, Tab/Task interaction, popup handling, page-residence distribution and optional video playback/keepalive | Reference | GPL-3.0 |
| [`Zzj-klwgxdz/UnipusAI`](https://github.com/Zzj-klwgxdz/UnipusAI) | `525c7ecfc2b46daa9877c93ad2f6422cf5084937` | Current Rust progress-leaf `tab_type`, required/minimum-score/time-window/statistic strategy, text/video mark-seen, generic ordered child answer body, content-derived judge types, external LLM and media transcription | Reference | GPL-3.0 |
| [Qiniu Kodo direct-upload API](https://developer.qiniu.com/kodo/1312/upload) | Page metadata updated `2025-12-16`; retrieved `2026-08-15` | The standard HTTP 200 response contains `hash` (Qiniu ETag) and `key`; upload-token policy may customize response content, so the donor-required exact key remains authoritative and a present bounded hash is optional stage evidence | Protocol documentation | Documentation only |

The Apache backend donor is the primary current HTTP reference. The MIT donor
is an independent route/schema cross-check and proves that completion,
progress and duration are separate observations. Its response contract also
explicitly identifies the study-record duration values as seconds and the
route caller binds query `id` to the numeric CourseResource ID. Its task runner
also corroborates the strict nonzero start/end availability check used before
interaction. Both backend response models expose the per-Unit progress
`open_id`, `tutorialId`, `unit_id` and `publish_version` route echoes; Asterism
validates all four at the native boundary and never persists the account or
Course-instance values. The GPL
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

The MIT `unitTaskSituation` model does not contain duration alone. On the exact
nested Task it also parses `finishProgress`, `required`, `scoreTaskFlag` and
optional `taskQuesTotalScore`. Asterism therefore retains those bounded facts
in a Provider-private Task study record and treats DurationRead as one exact
seconds projection, not as permission to discard the remaining donor semantics.

The same MIT donor treats
`studyRecord/totalAndUnitSituation?id={CourseResourceId}&appUserId={appUserId}`
as a separate Course/Unit read. The response carries Course total and Unit
finish progress, seconds, score and required state; Asterism therefore keeps a
dedicated Provider-private snapshot instead of collapsing those aggregates
into Task progress. Shared exposure remains a Core contract item.

After a successful mutation, the MIT donor also reads the exact
receipt-versioned user-module, treats summary `questionType=1` or `3` as a
scored task and pauses its runner when the returned average is zero. Asterism
keeps the useful protocol fact without coupling it to that GUI policy: the
same Course/Group/version-bound verification readback supplies an optional
fixed-point `score_avg`, including explicit zero, while receipts alone never
carry verified score.

That exact MIT response also carries receipt-scoped Task policy and submit
state. Asterism now retains the complete typed `strategyId`, `required`,
recording flags, Task minimum score, availability window, submit-window flags
and last-submit time in result-digest-bound Provider-private owners for the
ordinary, single-upload, compound-upload and compound-oral verifiers. Each
owner is returned only after its adapter-specific exact readback succeeds.
These observations neither authorize a completed-Task retake nor establish
precedence over Group, Unit or Course scoring facts; shared ingestion and
compound-attempt integration remain Main-owned contract gaps.

The MIT donor separately calls `courseStudyStrategy/detail` with the fresh
CourseResource strategy ID, then uses the returned per-Unit `requiredTask`
lists as its execution selection set. The same response evidences Unit and
Course windows, pass-score/score-type and unlock/scoring modes. Asterism ports
that route as an independent Provider-private policy snapshot; it does not
erase the current Rust donor's independently evidenced progress-leaf required
flag.

The mandatory one-time full sweep on 2026-08-14 re-read every default-branch
file, README/configuration and implementation path, MIT docs/inline examples,
all fetched tags/Releases, and every public issue for all four donors. Apache
has no tag, Release or public issue; MIT has four tags/Releases and 34 issues;
AutoPlayer has 14 tag refs, 13 Releases and 15 issues; current Rust has one
tag/Release and 18 issues. No donor revision or executable route changed.
`research/providers/uai/FULL_UPSTREAM_SWEEP.md` records the complete mapping.

That sweep also establishes negative history/retake facts. No donor enumerates
account-wide user-module versions or historical attempts. MIT reads one exact
Group only with the accepted submit version. Rust's `user_module.rs` is marked
pre-reserved, has no caller and uses an undocumented wall-clock suffix, so it
cannot authorize historical bootstrap. All three execution donors skip
completed Groups. Rust issue 4 requests repeat learning after a Course is shown
completed but supplies no implementation, route or eligibility response.

The receipt-bound user-module does contain child-level evidence beyond the
currently shared result: `answerList` carries `done`, `right`, submitted-answer
and version facts, while `quesData` carries exact submitted children. A UAI
module can contain multiple children represented by one Composite Question, so
the child rows cannot be assigned to Draft items by position. Owner-global
corpus ingestion therefore requires Main's lossless child-evidence contract;
overall `score_avg` remains task-level and never proves a child correct.

On 2026-08-13, all four UAI source checkouts were fetched with tags and compared
against their default remote branches. Apache `bef0d29155ce`, MIT
`590b4a58fe17`, AutoPlayer `cc6bdc86a13e` and current Rust
`40ead69c7dabf` were each at zero divergence; no new default-branch or tag delta
required an incremental port at this checkpoint. These revisions remain
reproducible audit snapshots, not permanent update ceilings.
The same four default branches and tag tips were fetched and confirmed at zero
delta again after the independent Course-progress checkpoint and the Micro
progress identity checkpoint on 2026-08-13.
All four default branches and tags were queried again at the BrowserBridge
exchange-ledger checkpoint on 2026-08-14. The same four revisions remained the
remote HEADs and no new tag appeared, so this checkpoint has no upstream delta.
The same fetch-plus-tag comparison was repeated after the subjective-empty
ResourceExecution checkpoint on 2026-08-14. Apache `bef0d29155ce`, MIT
`590b4a58fe17`, AutoPlayer `cc6bdc86a13e` and current Rust `40ead69c7dabf`
again matched their remote default branches with no new tag or capability
delta.
All four remotes and tag sets were refreshed again after the durable read-only
Question artifact/AnswerResolve handoff checkpoint on 2026-08-14. The same
four revisions remained current and no incremental donor delta was present.
The default branches and complete tag refs were queried again after the
artifact-bearing durable SubmissionExecute checkpoint on 2026-08-14. Apache
`bef0d29155ce`, MIT `590b4a58fe17`, AutoPlayer `cc6bdc86a13e` and current Rust
`40ead69c7dabf` still matched the recorded revisions, and no new tag or
capability/protocol delta required an incremental port.
The same four default branches and complete tag refs were refreshed after the
recoverable Browser residence command-artifact and owned-result checkpoint on
2026-08-14. All recorded revisions and tags remained unchanged, so no
incremental donor delta was present.
The four remote default branches and complete tag refs were queried again at
the persisted Browser exchange recovery checkpoint on 2026-08-14. Apache
`bef0d29155ce`, MIT `590b4a58fe17`, AutoPlayer `cc6bdc86a13e` and current Rust
`40ead69c7dabf` remained current, with no new tag or protocol/capability delta.
The unchanged AutoPlayer `cc6bdc86a13e` source was then re-read at the plan-v2
executor audit boundary. It confirms that `UAI_CMD` is iframe-side only, while
the top page owns direct Tab/Task/residence/video actions, and freezes the four
ordered iframe selectors plus its 30-second observer and 20×1.5-second retry
window. This is a clarified protocol boundary, not a new upstream revision.
The same source audit also confirms Course-level source-order
`menuList.slice(startIdx)` membership, restart from a newly selected ordinal,
fractional total/Micro/Tab/Task division and final-leaf `Math.round`. These
unchanged behaviors are now frozen in the Provider-local Course residence
batch fixture and plan; no new upstream revision or tag appeared.

All four remote default branches and complete tag refs were queried again at
the terminal residence/fresh-duration-readback checkpoint on 2026-08-14.
Apache `bef0d29155ce`, MIT `590b4a58fe17`, AutoPlayer `cc6bdc86a13e` and
current Rust `40ead69c7dabf` remained current, with no new tag or capability
delta requiring an incremental port.

The same four default branches and tag sets were rechecked after the exact
CourseResource-bound Task study-record owner checkpoint on 2026-08-14. All
recorded revisions remained remote HEAD and no new donor delta appeared.

All four default branches and complete tag refs were checked again after the
Core encrypted runtime-sidecar/UAI cursor handoff checkpoint on 2026-08-14.
Apache `bef0d29155ce`, MIT `590b4a58fe17`, AutoPlayer `cc6bdc86a13e` and
current Rust `40ead69c7dabf` remained unchanged.

The same remote HEADs and tag sets were queried after the consuming
result-inbox owner checkpoint on 2026-08-14 and remained unchanged; no
incremental Provider protocol delta was present.

All four remote HEADs and tag sets were checked once more after binding Core
result-inbox metadata to the consuming UAI owners on 2026-08-14; they remained
unchanged.

The same four default branches and complete tag refs were refreshed at the
credential-free Course child-plan checkpoint on 2026-08-14. Apache
`bef0d29155ce`, MIT `590b4a58fe17`, AutoPlayer `cc6bdc86a13e` and current Rust
`40ead69c7dabf` remained unchanged, so no incremental donor protocol or
capability delta was present.

All four default branches and complete tag refs were queried again at the
Core execution-plan artifact adapter checkpoint on 2026-08-14. The same
Apache `bef0d29155ce`, MIT `590b4a58fe17`, AutoPlayer `cc6bdc86a13e` and Rust
`40ead69c7dabf` revisions remained current, with no incremental donor delta.

The four default branches and complete tag refs were refreshed again at the
strict helper command-projection checkpoint on 2026-08-14. Apache
`bef0d29155ce`, MIT `590b4a58fe17`, AutoPlayer `cc6bdc86a13e` and current Rust
`40ead69c7dabf` remained unchanged, with no protocol/capability delta.

The same default branches and complete tag refs were queried at the typed
helper-result encoder checkpoint on 2026-08-14. All four recorded revisions
remained unchanged and no incremental donor protocol/capability delta appeared.

The four default branches and complete tag refs were refreshed again at the
closed typed helper-DOM recipe checkpoint on 2026-08-14. Apache
`bef0d29155ce`, MIT `590b4a58fe17`, AutoPlayer `cc6bdc86a13e` and current Rust
`40ead69c7dabf` remained unchanged. The pinned AutoPlayer source was re-read for
its exact 12-container/four-family Menu hierarchy and text fallbacks, Tab/Task
active tests, centered-scroll mouse-event order, popup selectors and 1-second
video poll/30-minute ceiling; no incremental donor delta was present.

The same four default branches and complete tag refs were queried at the
disposition-bound persisted cursor-result checkpoint on 2026-08-14. Apache
`bef0d29155ce`, MIT `590b4a58fe17`, AutoPlayer `cc6bdc86a13e` and current Rust
`40ead69c7dabf` remained unchanged, so strict result/session/digest/stage
adaptation introduced no donor protocol or capability delta.

All four default branches and complete tag refs were refreshed again at the
shared workflow-callback checkpoint on 2026-08-14. Apache `bef0d29155ce`, MIT
`590b4a58fe17`, AutoPlayer `cc6bdc86a13e` and current Rust `40ead69c7dabf`
remained unchanged; compact-plan/fresh-inventory recovery and shared result
construction introduced no donor protocol or capability delta.

The same four default branches and complete tag refs were queried at the
receipt-bound user-module policy-evidence checkpoint on 2026-08-15. Apache
`bef0d29155ce`, MIT `590b4a58fe17`, AutoPlayer `cc6bdc86a13e` and current Rust
`40ead69c7dabf` remained unchanged, so preserving the already-evidenced policy
fields introduced no upstream protocol or capability delta.

All four default branches and complete tag refs were refreshed again for the
completion-diagnosis audit on 2026-08-15. The same Apache `bef0d29155ce`, MIT
`590b4a58fe17`, AutoPlayer `cc6bdc86a13e` and Rust `40ead69c7dabf` revisions
remained current; the narrow `WindowClosed` mapping uses existing strategy
evidence and introduces no donor protocol delta.

The same four default branches and complete tag refs were refreshed at the
single-upload policy-evidence checkpoint on 2026-08-15. Apache
`bef0d29155ce`, MIT `590b4a58fe17`, AutoPlayer `cc6bdc86a13e` and Rust
`40ead69c7dabf` remained current, so reusing the receipt-bound policy parser
after exact upload-key verification introduces no donor protocol delta.

All four default branches and complete tag refs were refreshed again at the
compound upload/oral policy-evidence checkpoint on 2026-08-15. Apache
`bef0d29155ce`, MIT `590b4a58fe17`, AutoPlayer `cc6bdc86a13e` and Rust
`40ead69c7dabf` remained current; attaching the same owner after both exact
two-module readbacks introduces no donor protocol delta.

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
`instanceId=0` placeholder question rows. Asterism retains that as a separate
placeholder wire mode instead of treating it as evidence against the marker,
uses the five labels only to identify scan-time candidates, and requires a
fresh exact text/video progress leaf before mutation.
The same current progress model exposes per-leaf `required`, `min_score_pct`,
`start_time`, `end_time` and `statistic_mode_out`; Asterism retains those facts
for Task selection and applies the donor time-window rule to every native
mutation rather than relying on stale tree labels.
The same evidenced strict window supports one conservative post-verification
diagnosis: an exact incomplete Group observed at or after its positive close
time is `WindowClosed`. No donor source gives causal meanings to individual
`pass`/`pass2`/`perm` zeros, so score, duration, prerequisite, child and review
diagnoses remain intentionally unmapped.

The 2026-08-13 re-audit also checked the current Rust donor's media path. It
downloads audio/video and invokes local Whisper transcription for answer
generation, but adds no new UAI platform route or mutation shape beyond the
already audited encrypted content/answer and media-source fields. Asterism
therefore keeps the bounded opaque attachment/transcript boundary in the
Provider; the Task-bound downloader, model credential and external
AnswerResolve execution remain a shared Core contract gap rather than a
Provider-side omission. Its subtitle helper also strips WEBVTT/SRT headers,
numeric cue IDs and timestamp rows before transcription; the Provider applies
that evidenced normalization to embedded WEBVTT without retaining timing noise.
The current implementation at `525c7ecfc2b4` still routes `.vtt`/`.srt`
downloads directly through that line normalizer before the ffmpeg/Whisper
branch and trims cached/inferred text. Asterism therefore also permits an exact
bound subtitle response to enter the same bounded normalization, while audio
and video remain behind the shared transcriber/model provenance contract.
Its answer path iterates every module `media_sources` entry in source order,
keeps each nonempty transcription and joins them only after the complete pass.
Asterism freezes that complete ordered fetch set and its digest in the Provider;
failure/retry and transcript aggregation remain explicit shared policy rather
than being silently copied from the donor's best-effort loop.

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
`translation` and `revise-mistake`, plus discussion direct empty marking and
topic/reply APIs, exit-ticket/oral empty submission and CMS-token/object-store upload. These are
implementation scope. Where the shared Core cannot yet express a reply draft,
artifact handle or external resolver, that is a Core Gap rather than a Provider
policy exclusion.

For pure-study Groups, MIT/current donors evidence the no-Question marker while
Apache's active generic path evidences a separate placeholder body. Its
`parse_base_list` pads a short comma-split base list by repeating the final
type and truncates an overlong list to `question_num`; the builder then emits
one `instanceId=0` empty Question and ordered judge per expanded type. Asterism
retains both through a frozen runtime choice and replaces Apache's captured
Course judge version with fresh Course-progress evidence.

For one exact `discussion` Group, the Apache runner exposes two independent
direct-empty branches in addition to topic/reply automation. Its skip-content
branch sends the no-Question `submitType=2` marker; its allow-empty branch uses
the generic builder's `instanceId=0`, one empty child, courseAnswer map and
`discussion` judge under `submitType=1`. Asterism retains both behind a frozen
versioned runtime choice and replaces the donor's captured judge Course version
with the current independently refreshed publish version.

The Apache runner separately declares `SUBJECTIVE_TYPES={"short_answer"}` and,
when `SUBJECTIVE_ALLOW_EMPTY` is enabled, invokes its generic simple submit
builder with an empty answer list. That produces one `instanceId=0` empty child
and a hyphenated `short-answer` judge for each declared Question. Asterism maps
this evidence to an independently authorized subjective-empty ResourceExecution
while retaining the same Task's ordinary question, answer, submission and
receipt-versioned verification capabilities. The captured Course judge version
is replaced by the current independently refreshed publish version.

The same Apache runner recognizes the exact compound family
`basic-scoop-content,oral-sentence`, reads one encrypted standard-answer array
for both native modules and submits both slots in one `submitType=1` body.
Its empty-oral configuration proves the scalar-empty fallback and its generic
builder proves an empty-array child when the encrypted row contains an empty
child. Asterism freezes that observed representation and the explicit oral
instance together with the matching Draft, current Task fingerprint and Course
publish version. The same donor's simple builder establishes the stable
per-child rule: truthy `answers` wins over truthy `value`, `answersExtra` maps
to that submitted child's `extra`, and the judge value joins text arrays or
stringifies a scalar. Its compound builder accidentally indexes a per-module
array by child ordinal. Asterism does not reproduce that local bug: the
clean-room mapping retains each oral child's own bounded value/extra and uses
only donor-stable judge encodings. Receipt readback must preserve both fields
exactly; object values whose Python-specific string representation is not a
stable protocol encoding still fail closed.

The MIT donor also treats `video-popup` as an answer-bearing type, reads the
first standard-answer value from every child and selects `submitType=2` because
the exact base remains a study mode. The current Rust donor independently
parses `video-popup` module/child `replyType`, solves its choice/text/banked
children and serializes their exact judge labels. Asterism combines those
compatible facts: content determines the typed answer shape, all answer rows
are retained, and an all-`video-popup` plan uses type 2 rather than the empty
preset body.

The Apache donor's single `multiFileUpload` path explicitly obtains the CMS
grant, uploads one MP3, then submits `instanceId=0` with one completed child
whose value array contains the returned object key. Its compound
`multichoice,multiFileUpload` path submits both modules together. Asterism maps
the donor's camel-case type to that exact canonical spelling at the Task-tree
boundary; the general lowercase normalization must not turn it into an
unreachable `multifileupload` before either upload gate. The single-upload
child maps into the current Rust donor's minimal ordered
question/judge envelope and fresh Course publish version, without copying the
Apache donor's fabricated score maps. The compound form remains atomic and
cannot be decomposed into an upload mutation plus a later unrelated answer.
The Provider-private compound plan now preserves that boundary with one
validated ordinary sub-draft and one bound uploaded artifact; shared Core must
still persist them as one compound Draft/Attempt before capability exposure.

The sanitized protocol-observation checkpoint on 2026-08-15 refreshed all four
recorded donor heads and tags. Apache remained `bef0d29155ce`, MIT remained
`590b4a58fe17`, AutoPlayer remained `cc6bdc86a13e` and Rust remained
`40ead69c7dab`; no new tag or protocol delta appeared. The observation schemas
therefore describe local fail-closed diagnostics only and do not extend donor
capability evidence.

The next ordinary delta check later on 2026-08-15 found one new Rust default-
branch commit, `525c7ecfc2b46daa9877c93ad2f6422cf5084937`. It changes only the
donor CLI's local `course_id`-to-Chinese-display-name heuristic in
`src/api/course.rs`; no route, response field, authentication, task,
submission, media or Browser behavior changed, and tag `3.0` remained the only
release tag. Asterism already reads bounded authoritative Course and
CourseResource `name` values from `getCourseListByStudent`, so it deliberately
does not port this guessed code-name mapping. Apache, MIT and AutoPlayer default
revisions and complete tag sets remained unchanged. Their visible issue lists
still contain no new capability-bearing issue beyond the previously audited
surfaces.

The upload stage-output design checkpoint on 2026-08-15 queried all four
default refs and complete tag refs again. Apache remained `bef0d29155ce`, MIT
remained `590b4a58fe17`, AutoPlayer remained `cc6bdc86a13e` and Rust remained
`525c7ecfc2b`; no new donor commit or tag changed upload, submission, media or
Browser behavior. The pinned Apache upload implementation was re-read at this
checkpoint: it consumes only the exact `key` from the Qiniu response. Qiniu's
current primary direct-upload documentation additionally defines the normal
HTTP 200 JSON as `hash` plus `key`, with `hash` serving as the Qiniu ETag.
Asterism may preserve a bounded present hash as encrypted stage evidence, but
must not require it when the CMS-issued upload policy customizes the response,
use it instead of the exact donor-required key, or promote the same mutation
response to independent verification.

The final-upload retry-sequence checkpoint later on 2026-08-15 queried the
same four default refs and complete tag refs. Apache remained
`bef0d29155ce`, MIT remained `590b4a58fe17`, AutoPlayer remained
`cc6bdc86a13e` and Rust remained `525c7ecfc2b`; no new donor commit or tag
changed the audited final-submit retry behavior. The bounded sequence therefore
retains Apache's exact one delayed retry after numeric `600001`/`600002`
rejection and does not infer retry authority from generic rejection,
authentication failure, malformed response or ambiguous transport.

The discussion conditional-sequence checkpoint later on 2026-08-15 repeated
the four default-ref and complete-tag queries. Apache remained
`bef0d29155ce`, MIT remained `590b4a58fe17`, AutoPlayer remained
`cc6bdc86a13e` and Rust remained `525c7ecfc2b`; no donor or release delta
changed the audited reply, exact readback or separate Group-completion order.
The Core projection therefore retains two non-replayable mutation ordinals and
uses the already audited current-user/content readback only as the explicit
gate between them.

The persisted upload retry-deadline checkpoint later on 2026-08-15 queried all
four default refs and complete tag refs once more. Apache remained
`bef0d29155ce`, MIT remained `590b4a58fe17`, AutoPlayer remained
`cc6bdc86a13e` and Rust remained `525c7ecfc2b`; no donor or release delta
changed the exact numeric `600001`/`600002` classification or 120-second delay.
The new durable deadline is therefore a faithful Core persistence of existing
donor evidence rather than a newly inferred retry policy.

The upload recovery-verification audit later on 2026-08-15 repeated those four
default-ref and complete-tag checks with the same unchanged revisions. No donor
added a version-free user-module read or an object-key-independent verification
surface. Exact recovery therefore still needs Apache/MIT's accepted submission
version plus the immutable single/compound answer material; a response hash or
fresh completion progress is not substitute evidence.

The accepted final-result state checkpoint later on 2026-08-15 queried every
default ref and complete tag ref again. Apache remained `bef0d29155ce`, MIT
remained `590b4a58fe17`, AutoPlayer remained `cc6bdc86a13e` and Rust remained
`525c7ecfc2b`; their tag sets were unchanged. The Provider codec therefore
preserves the already audited code-0 submission version and exact request/body
response lineage without adding donor semantics. Its encrypted state is a
future atomic recovery input, not a substitute for the exact final plan or
receipt-versioned readback.

The compound final-plan binding checkpoint later on 2026-08-15 re-used the
same immediately preceding unchanged four-donor head/tag query and re-read the
Apache atomic compound branch. Because the donor emits the ordinary selected
answer and upload key in one body, Asterism now hashes the complete ordinary
child/judge semantics directly into the sequence binding instead of relying on
the Draft identifier alone. This strengthens local recovery identity without
adding or changing donor wire behavior.

The accepted-result verification-adapter checkpoint on 2026-08-16 queried all
four default refs and complete tag refs again. Apache remained
`bef0d29155ce`, MIT remained `590b4a58fe17`, AutoPlayer remained
`cc6bdc86a13e` and Rust remained `525c7ecfc2b`; their tag sets were unchanged.
No donor added a version-free readback, changed the atomic compound answer/key
shape or supplied a generic parent-batch upload recovery protocol. UAI now
preserves the accepted timestamp and converts only an exact final-plan-bound,
receipt-versioned readback into mutation verification; this is stricter local
recovery binding over the existing donor protocol, not a new wire capability.

The complete-final-plan state checkpoint later on 2026-08-16 repeated all four
default-ref and complete-tag queries with the same unchanged revisions. No
donor added recoverable platform storage for the private object key or compound
selected answer. Asterism's new bounded `uai.upload.final-plan-state.v1` codec
therefore freezes existing donor-required mutation input and the already
materialized exact request digest, then requires it to reproduce the
independently stored compact artifact and sequence digests. It adds no upload
route, retry rule or verification shortcut.

The object-state checkpoint later on 2026-08-16 reused the same immediately
preceding unchanged four-donor head/tag query and re-read the Qiniu primary
response contract. The donor still requires only the exact returned key, while
the primary API documents optional normal `hash`/ETag evidence. Asterism now
preserves a valid bounded present hash and exact response digest in typed
Provider state, keeps key-only customized responses compatible and continues
to treat the response as a mutation receipt rather than independent readback.

The grant-state checkpoint later on 2026-08-16 queried all four default refs
and complete tag refs again. Apache remained `bef0d29155ce`, MIT remained
`590b4a58fe17`, AutoPlayer remained `cc6bdc86a13e` and Rust remained
`525c7ecfc2b`; their tag sets were unchanged. No donor added a token TTL,
independent grant verification or cross-request grant-recovery protocol. The
new bounded `uai.upload.grant.v1` codec therefore preserves the already audited
token/key prerequisite together with exact request/response and Task/artifact
lineage; it adds no donor wire behavior or Task mutation evidence.

The input-state checkpoint later on 2026-08-16 reused the same immediately
preceding unchanged four-donor head/tag query. No donor introduced a separate
artifact persistence service or weakened the atomic mixed choice/upload
requirement. Asterism's compact binary `uai.upload.input.v1` therefore preserves
the already bounded MP3 bytes plus exact single-Task or immutable compound-Draft
binding without changing the donor upload protocol. It is local recovery state,
not evidence that Core has persisted or authorized an Attempt.

The cross-stage recovery checkpoint later on 2026-08-16 reused that unchanged
four-donor head/tag result. No donor added an object-stat readback or any rule
allowing a new token to inherit an older Qiniu success. UAI therefore reconnects
the existing audited intent/grant/object lineage and exact multipart digest
locally, drops the reconstructed request without dispatch and enters only fresh
final-plan preparation. This adds recovery validation, not a replay authority
or new donor capability.

The end-to-end final-plan recovery checkpoint later on 2026-08-16 reused the
same unchanged donor query. No upstream exposed an alternative account-free
final-submit identity or allowed a self-consistent final plan to detach from its
object-upload predecessor. UAI now rebinds the decoded private final plan to the
full predecessor chain and exact current Course/openid request before accepted
readback; this remains local fail-closed recovery over unchanged donor routes.

The recovered-readback entry checkpoint later on 2026-08-16 reused the same
unchanged upstream result. It narrows local API authority only: raw
result-to-plan readback is no longer externally callable, while the fully
recovered single and compound owners exercise the already audited
receipt-versioned verification routes. No donor behavior or verification claim
changed.

The compound-oral result-digest checkpoint later on 2026-08-16 reused the same
unchanged donor result. It corrects local evidence ownership only: the exact
validated readback digest now survives even when optional strategy and score
summary fields are absent. No route, answer shape, score rule or completion
claim changed.

The compound-oral semantic-binding checkpoint later on 2026-08-16 reused the
same unchanged donor result. It separates existing immutable matching/oral plan
identity from dynamic Course/openid wire identity and adds no route or answer
shape. This is local persistence hardening for the already audited atomic body.

The compound-oral compact-artifact checkpoint later on 2026-08-16 reused that
unchanged donor result. The new credential-free projection carries only stable
Task/Draft/version/digest identity and omits all executable matching/oral and
dynamic account material. It adds scheduling representation, not donor
behavior.
