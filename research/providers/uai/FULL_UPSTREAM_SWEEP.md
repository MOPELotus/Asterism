# UAI one-time full upstream sweep

Audit date: 2026-08-14. This is the mandatory full re-audit required by
`research/providers/NEXT_CHECKPOINT.md`. It re-enumerates the complete current
surface of every recorded UAI donor instead of considering only changes after
the pinned revisions. No real account or mutation was used during this pass.

## Evidence enumerated

| Evidence line | Material re-read |
|---|---|
| `create-try-now/AutoFinish_UxiaoyuanAI` | Complete `main` tree, README, every Python setting and implementation branch, and the absence of tags, Releases and public issues |
| `Duster-Cule/UnipusHelperPro` | Complete `main` tree, README and `doc/`, every Java source/inline response example, all four tags/Releases (`v1.0.0..v1.0.3`) and all 34 public issues |
| `uxudjs/UnipusAIAutoPlayer` | Complete `main` tree, multilingual README, full userscript/control surface, all 14 tag refs, 13 Releases and all 15 public issues plus relevant page-source attachments/comments |
| `Zzj-klwgxdz/UnipusAI` | Complete `master` tree, README/config example, every Rust source, the independent `release/v2.4` branch, tag/Release `3.0` and all 18 public issues |

The refreshed default revisions remain:

- Apache `main@bef0d29155cef727e05ba6b72336ee212c94fe84`;
- MIT `main@590b4a58fe175240fe9a08fdd69948effcf4f193`;
- AutoPlayer `main@cc6bdc86a13e7c80a54dff50819607a488ed952e`;
- Rust `master@40ead69c7dabf7a2f3a215ff69f3feba73a736f6`.

No default-branch or tag delta is available to port. The newest AutoPlayer
source is beyond release `v5.2.13`; the Rust default branch is beyond release
tag `3.0`. The full default branches, not release archives alone, remain the
current audit authority.

None of the four donors contains an automated protocol-test or sanitized
fixture suite. Reusable evidence consists of implementation, inline response
examples, README/configuration, release/tag history and issue payloads or
attached page sources. Asterism fixtures remain clean-room synthetic values.

## Complete platform capability surface

| Donor behavior | Current Asterism mapping | Classification |
|---|---|---|
| Password/JWT login, imported browser credentials and user-info validation | Authentication, Capture recipe and stored-session validation | Implemented at offline/native boundaries; Capture execution and live validation remain shared gates |
| CourseResource, detail/tree, Course progress, per-Unit progress and required policy | CourseInventory, TaskInventory, TaskDetail, ProgressRead and Provider-private policy | Implemented; shared Course/Unit aggregate and policy scheduling contracts remain Core gaps |
| Course/Unit/Task study records | Provider-private aggregate/Task records plus DurationRead projection | Implemented without conflating progress, score and seconds |
| Encrypted content and standard answers | QuestionInventory/Parse, AnswerResolve and media continuation | Implemented for every audited typed shape; external downloader/model execution remains shared |
| Ordinary ordered answer submission and receipt-versioned user-module readback | SubmissionBuild/Execute/Verify | Implemented with fresh task/window preflight and no ambiguous replay |
| Pure-study, exit-ticket, direct discussion, subjective-empty and oral placeholder completion | ResourceExecution plus fresh ProgressRead verification | Implemented as distinct audited mutation families |
| Discussion topic/reply/readback then Group completion | Provider-private two-mutation workflow | Implemented to the Provider boundary; shared compound Draft/attempt persistence remains missing |
| CMS grant, Qiniu upload and single/compound upload submit/readback | Provider-private upload workflows | Implemented to the Provider boundary; shared artifact and compound-attempt ownership remains missing |
| External OpenAI-compatible answering | Answer candidate/resolver orchestration | Core gap: Provider exposes content and typed answer inputs but does not own model credentials or resolver policy |
| Audio/video/subtitle extraction and local Whisper | bounded media manifest/transcript parsing | Provider parsing is implemented; authenticated downloader, ffmpeg/Whisper execution and model/cache policy are shared gaps |
| Rust `dump-text`, `progress`, `group`, `debug`, `test-types` and `transcribe` commands | existing inventory/question/media primitives | CLI/debug/export composition, not additional UAI wire protocols |
| Rendered Micro/Tab/Task traversal, popup handling, residence and video playback | DurationReport BrowserBridge plan/cursor/helper recipe/callback | Provider protocol is implemented; session injection and actual DOM dispatch remain shared executor gaps |
| AutoPlayer page-source download/feedback UI | local diagnostic export | Application-only behavior; it carries no remote learning capability or mutation authority |

The sweep found no previously unrecorded UAI route family. Apache's objective,
subjective, discussion, exit-ticket, oral, upload and compound branches; MIT's
progress/policy/study-record/score path; Rust's child-level answer/media path;
and AutoPlayer's full rendered lifecycle are all already mapped above.

## Complete settings and control surface

| Donor control | Semantic mapping | Current decision |
|---|---|---|
| Apache AI URL/model/key/max tokens/timeout/retry mode and per-family prompts | external resolver configuration and bounded failure policy | Shared AnswerResolve/model policy gap; secrets never become Provider runtime settings |
| Rust API key/base URL/model/max tokens/temperature/fallback | same external resolver boundary | Shared gap. Random/placeholder fallback is donor behavior, but requires an explicit provenance-bearing resolver policy; UAI must not silently label it ProviderNative |
| Rust Whisper enabled/model/language and media cache | transcription/downloader execution | Shared media execution/cache policy gap; Provider already supplies the bounded attachment/transcript facts |
| Apache subjective/discussion/exit-ticket/upload/oral/unknown/compound switches | explicit capability authorization and family-specific mutation choice | Implemented families stay separate. Unknown generic submit remains fail-closed until its actual content/wire is evidenced; product authorization must not erase supported families |
| Apache `COOLDOWN_COUNT=5`/`COOLDOWN_SECONDS=120`, MIT five submits/minute plus two-minute throttle handling, Rust `interval_ms=1200` | Provider/account mutation scheduling and typed retry delay | Shared scheduler/attempt policy gap; Provider returns `600001/600002` without an implicit replay |
| MIT required-only selection and Rust `learn_all`/`learn_all_compulsory_course` | complete inventory plus required-task selection | Provider retains all required/policy facts; shared batch/scheduler policy must select all versus required without pruning inventory |
| AutoPlayer start Micro, total minutes, play-video, pause/resume and mid-run restart | frozen Course batch start, residence seconds, video flag and explicit cursor control | Provider protocol implemented; active UI control ownership and dispatch remain shared executor work |
| Asterism provider/account concurrency and scan interval | Core scheduling | Implemented Asterism-owned controls, not donor wire fields |

The current UAI runtime schema exposes residence/video and the two exact
marker/placeholder choices. Family enable/disable remains capability/run
authorization. External resolver, transcription, all-vs-required selection,
throttle and active Browser control are correctly not hidden inside mutable
Provider-local state, but Main still needs their shared durable contracts.

## Issues, examples and drift evidence

MIT issues 11/38 corroborate required-list and Course-shape variation; issue 22
corroborates throttling; issue 43 demonstrates empty answer-array drift. The
remaining MIT issues are authentication, packaging, Java/UI or unsanitized
generic failures and establish no extra route.

AutoPlayer issues 9/12/14-17 and their attached page sources document continuing
directory-selector variation. Issue 17 specifically reports that a broad
`menu` selector can select a video-player Menu rather than the Course menu.
Issues 15/16 confirm that the Course list page and rendered learning page are
different origins/contexts; the maintainer requires entry into the learning
page. Popup and video comments corroborate the already frozen popup/video
lifecycle. These are drift/live-validation evidence, not a second browser
capability.

Rust issues 6/7/10/17 corroborate child cardinality, prompt/content and
banked-cloze drift; issues 12/16 corroborate transcription environment limits.
Issue 4 asks for an ability to repeat a Course shown as completed, but supplies
no route, response, maintainer implementation or eligibility fact. It is a
feature request, not retake protocol evidence.

The Apache donor has no public issue, tag or Release surface. Its complete
single-file implementation and README are the audit boundary.

## Answer evidence and read-only bootstrap

The owner-global Answer Evidence Corpus is Main-owned. UAI supplies these
facts, which must remain independently bound:

| Shared concern | Audited UAI fact | Consequence |
|---|---|---|
| Historical enumeration | No donor exposes an account-wide attempt/version list or a Course history/result collection route | First binding may read Course/Task/progress inventories, but cannot claim a complete answer-history harvest |
| Exact historical read | MIT reads only `user_module/{course}/{group}-{receiptVersion}` immediately after accepted submit | This is safe and exact for incremental verification, but the receipt cannot be guessed during bootstrap |
| Provisional current-record read | Rust contains an unused `fetch_my_records(group)` module marked `pre-reserved`; it sends the current wall-clock suffix and reads `data.rows` | No caller, fixture, version semantics or pagination establishes reliable bootstrap authority; live sanitized evidence must prove it before implementation |
| Submitted answer | exact `quesData` module identities, submitted context, child `isDone` and values | Existing SubmissionVerify binds these to the immutable Draft and may supply provenance for incremental corpus ingestion |
| Per-child judgement | `__SUMMARY__.answerList` exposes `done`, `right`, `questionType`, submitted-answer and protocol versions | The field is protocol evidence, but its entries can be child-level while the shared Draft is module-level Composite. Do not map it by row number until Core/Domain has a lossless child-evidence contract |
| Official/reference answer | encrypted standard-answer route exposes ProviderNative answers for the current Task | It is a current resolver read, not a historical result enumeration surface; corpus storage/matching remains Core-owned |
| Score | counted `questionType=1|3` authorizes exact `score_avg` in the same bound readback | Preserve fixed-point zero through 100, but never promote overall score into per-Question correctness |
| Receipt policy | exact result may carry strategy identity, required/recording flags, Task minimum score, availability and submit state/time | Preserve the complete typed facts behind a result-digest-bound Provider owner; all-absent legacy policy is compatible, partial policy fails closed, and recording flags grant no retake authority |

Normal SubmissionVerify is therefore the immediately usable incremental feed:
it already proves the exact submitted values and optional score. Corpus append
still needs a shared evidence/provenance result contract, and verified-correct
promotion additionally needs the shared child mapping above. A first-bind
bootstrap must return a partial report rather than invent versions or mutate a
Task to reveal results.

## Strict Completion, score improvement and retake

Strict Completion and Score Improvement are independent and default-enabled in
Core; UAI contributes facts rather than owning either state machine.

- Strict Task completion is fresh `pass=1 && pass2=1 && perm=1`. A receipt,
  submitted-answer equality, score, `finishProgress`, duration or one of the
  three flags alone is not completion.
- The only audited incomplete-cause mapping is an exact Group whose fresh
  positive close time is at or before the following progress observation;
  ResourceExecution may report `WindowClosed`. Donors do not define causal
  meanings for individual zero completion flags, and SubmissionVerify cannot
  expose its private policy through the shared snapshot, so every other UAI
  completion diagnosis stays unknown.
- Course/Unit `finishProgress=100` is an aggregate checkpoint. It cannot replace
  exact Group completion when the policy is operating on one Task.
- Group `min_score_pct`, user-module `task_mini_score_pct`, Unit `passScore` plus
  `scoreType`, Course `scoringMode`, and Task `scoreTaskFlag` are separate facts.
  No donor establishes a universal precedence rule, so live/provider-specific
  policy reconciliation must fail closed rather than choosing one threshold.
  The ordinary verifier now preserves the complete receipt-bound user-module
  fact and exact-result digest Provider-privately; this does not choose a shared
  precedence rule, and upload/compound evidence projection remains separate.
- Strict Completion may submit another normal Attempt only while a fresh Group
  remains incomplete, is exact `tab_type=task`, and is inside its availability
  window. The usual immutable Draft, mutation ledger and verification rules
  still apply.
- All three execution donors skip already completed Groups. None implements a
  reset, redo, retake creation call, remaining-attempt count or explicit
  completed-Task eligibility response. Score Improvement must therefore report
  retake unavailable/unknown for UAI and must not submit a completed Group.
- `record_every_submit` and `record_max_submit` are observed strategy booleans,
  not retake permission. Accepted submit versions prove multiple record shapes
  can exist, not that a completed Task can be reopened.
- A failed or lower score-improvement attempt could never revoke the separately
  verified completion state; this invariant remains Core-owned even after a
  future UAI retake route is established.

## Shared gaps reported to Main

1. There is no owner-global corpus, evidence/provenance record, conflict model,
   semantic matcher or atomic SubmissionVerify ingestion contract.
2. There is no durable first-bind read-only harvest job/partial report contract.
   UAI additionally lacks evidenced account-wide history enumeration.
3. `SubmissionVerificationSnapshot` cannot expose submitted values plus
   per-child judgement/protocol versions independently of the Draft's
   module-level Question. Shared child/Composite evidence modeling is required.
4. Core has no separate default-on Strict Completion and Score Improvement
   state machines with immutable resolved policy snapshots and Provider retake
   availability. UAI currently supplies strict completion but no retake.
5. Course/Unit/Group scoring fields lack a shared typed policy contract and
   evidenced precedence rule.
6. External model/transcription, all-vs-required selection, throttle scheduling,
   compound attempts and active Browser dispatch remain the previously recorded
   shared boundaries.

## Re-audit outcome

No new executable UAI wire capability was omitted from the Provider. The sweep
did recover important shared requirements and negative protocol facts: the
unused Rust current-record helper is not a history API, summary judgement is
child-shaped, the completion and score thresholds are independent, and no
donor permits a completed-Task retake. These facts are now explicit rather than
being inferred from score or product policy.

This one-time full-sweep requirement is complete for UAI. Future meaningful
checkpoints return to ordinary revision/delta checks while Provider work
continues through the recorded Core, executor and live-validation boundaries.
