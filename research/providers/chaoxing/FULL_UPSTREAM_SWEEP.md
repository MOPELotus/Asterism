# Chaoxing one-time full upstream sweep

Audit date: 2026-08-14. This is the mandatory full re-audit required by
`research/providers/NEXT_CHECKPOINT.md`. It re-enumerates every recorded donor
from its complete current default-branch surface rather than reviewing only
commits after the previous pin. No donor or Asterism path was live-tested with
a real account during this pass.

## Evidence enumerated

| Evidence line | Re-read surface |
|---|---|
| `Samueli924/chaoxing` | Complete `main` tree, README/config examples, all Python implementation and tests, 45 tags, Releases and public issues |
| `surinrasu/CxKitty` | Complete `main` tree, README/config, all Rust implementation/examples, public issues, and the absence of tags/Releases |
| `iwillwill-ALLWILL/chaoxing-agent-skill` | Complete `main` tree, skill/config/reference documents and scripts, public issues, and the absence of tags/Releases |
| `ocsjs/ocsjs` | Complete default `4.0` tree, README/config, Chaoxing project code, shared worker/resolvers/tests, 321 tags, Releases and public issues |
| `LangHY/chaoxing-exam` | Complete `main` tree, README, scripts, result/retake notes and examples, public issues, and the absence of tags/Releases |
| `CodFrm/cxmooc-tools` | Complete historical `master` tree, README/config, Chaoxing task/question/result implementation, 29 tags, Releases and public issues |

The refreshed symbolic revisions are unchanged:

- `Samueli924/main@9699e632b492cdc55ea35a1ab05b6dfcbfb7cf70`, latest release `v3.1.4`;
- `surinrasu/main@1589eac9c07c4bab71f79d762b45210643dd537d`;
- `iwillwill-ALLWILL/main@f72619a0b36996d27d00577015663ec39e782500`;
- `ocsjs/4.0@890686a5e54f9a6d52d1169bae9ea5971e0863c7`, latest release `4.15.3`;
- `LangHY/main@14e1dfd9cf11cd54dabb494dd01e318856d9b8d3`;
- `CodFrm/master@2b81f7b55a68ea2ceb6ea0312a6791ebf3ed3dc5`, latest release `v2.5.0`.

## Complete donor capability and settings surface

| Donor behavior | Asterism mapping | Classification |
|---|---|---|
| Password/Cookie login, session validation, Course/folder/Chapter inventory | Authentication, CourseInventory and TaskInventory | Implemented; QR remains a shared interactive-auth gap |
| Video, Document, Read, Live, Audio and hyperlink cards | Resource inventory/execution families | Video, Audio, Document, Read and Live checkpoints implemented with fresh-card verification; hyperlink still requires a Browser fixture and bounded execution verification |
| Chapter Work, independent Work and formal Exam discovery | Distinct Task source/assessment facts | Implemented for audited inventory routes |
| Work/Exam question read and mutation | QuestionInventory/Parse and Submission capabilities | Native subsets implemented; rendered long-tail controls require BrowserBridge |
| `submit` plus `cover_rate=0.9` | minimum submission coverage over the complete snapshot | Shared Core Gap; current Core requires one selected candidate per Question |
| OCS `save|nomove|force|10..100` upload policy | save-only versus thresholded/final submit | Corroborates separate mutation intent and coverage policy; `force` is not a default bypass |
| Samueli rollback submit after a failed prerequisite Work | explicit prerequisite/unlock execution policy | Shared policy gap; must never silently lower normal coverage |
| Agent retake `pass_score=60`, `high_score=90`, `retry_max=3` | Strict Completion and Score Improvement state machines | Shared attempt/policy gap; no implicit retake |
| CxKitty Exam temporary save/final submit and face/captcha gates | durable Exam attempt/save/final operations plus HumanRequired | Native subset implemented; browser challenge paths remain pending |
| Result-page answer read and historical answer reuse | owner-level Answer Evidence Corpus and read-only bootstrap | Provider parsers exist only for bounded current result shapes; shared corpus/bootstrap lifecycle is missing |
| External Tiku/AI/wrappers/manual sources | ordinary answer candidates with retained provenance | Core concern; never relabel as ProviderNative |
| Random/fuzzer fallbacks and random in-video answers | none | Intentionally excluded: Asterism never invents an answer |
| Notifications, updater, UI panels, download/export and local session UX | application behavior | Not a Chaoxing protocol capability |

Additional executable donor behavior recovered by the sweep includes Samueli's
post-task learning-count loop, normal/photo/location sign-in methods, and
Video-to-Audio fallback; CxKitty's QR session, face upload and independent Exam
export; OCS's PPT/book page turns, timed Read, in-video questions and repeated
study controls; and historical `cxmooc-tools` Audio, Document, result collection
and course-wide Exam lookup. They remain explicit capability candidates rather
than being treated as out of scope. Audio is now implemented only from OCS's
explicit `insertaudio` card fact and never through failure-driven fallback.
Sign-in, learning-count manipulation, in-video Questions and Browser-only
long-tail interactions still need separate fresh fixtures and durable
mutation/verification designs before registration.

## Observed Question boundary

The combined donors establish these currently observed families:

| Chaoxing code/shape | Donor evidence | Current boundary |
|---|---|---|
| `0` single, `1` multiple, `3` true/false | all current donors | Parsed, native mutation and strict visible-answer verification |
| `2` fill | CxKitty, OCS, agent skill and `chaoxing-exam` | Parsed and native Chapter/Exam value encoding exists; result harvesting stays blocked on exact result DOM because fill can be pending manual grading |
| `4..9` text/short/reference answers | OCS and browser donors | Parsed as ShortAnswer; exact Browser/editor mutation and reference-answer provenance still need fixtures |
| `10` | OCS treats as text completion; existing Asterism evidence maps it Composite | Donor-variant conflict: retain fail-closed submission until a route-specific fixture disambiguates it |
| `11` matching/line | OCS current implementation | Parsed as Matching; rendered select-box mutation needs a bounded BrowserBridge recipe and result fixture |
| `14` cloze and `15` reading composite | OCS current implementation | Parsed as Composite; child Questions/shared context/options need shared Domain modeling before answer evidence can be lossless |
| shared-option Questions | OCS issue #297 | Confirmed unsupported boundary; no opaque metadata workaround |
| in-video Questions | OCS and open issues | Observed Resource subflow; random donor strategy is rejected, exact Question/readback fixtures are missing |

The Provider unit matrix now pins every observed numeric classification and
keeps unknown codes as `QuestionKind::Unknown`. This does not claim mutation
support. A parsed type lowers coverage when unanswered; it must not disappear
from the complete snapshot or receive a guessed value.

## Result evidence and read-only historical bootstrap

The donors expose several facts that must remain separate:

- a completed task/card or final score proves task state, not any answer;
- `我的答案` is the user's submitted value, not necessarily correct;
- `正确答案` is official ProviderNative evidence when bound to an exact
  Question and current-attempt option mapping;
- a per-Question correct marker can promote the submitted value to
  VerifiedHistorical evidence even if no official standard is shown;
- short/reference answers may be manually graded and must preserve that weaker
  label rather than becoming a unique correct answer.

Samueli issue #607 explicitly requests reading already-passed Questions into
the cache. OCS issue #250 requests importing correct answers, while current OCS
already feeds completed worker results into its local cache. The agent skill
scans result pages for score and empty `我的答案`; `chaoxing-exam` reaches
`selectWorkQuestionYiPiYue`, reads standards, and can open a retake. Historical
`cxmooc-tools` also contains result-page collection. Together these establish a
read-only historical bootstrap direction, but not one universal route.

For Chaoxing, account binding may safely enumerate already completed Chapter
Work, independent Work and Exam rows, then read only result routes that can be
strictly rebound to owner/account/course/task/attempt. It must never call
`api/work`, `phone/start`, `redoTest`, `reTest` or any route which creates an
attempt. Normal SubmissionVerify can feed the same corpus incrementally after
fresh per-Question evidence. The current shared model has no owner-level corpus,
evidence status independent of `AnswerSource`, or read-only harvest job/result
contract, so Provider registration is blocked even though a strict Chapter
choice/true-false standard parser already exists.

## Completion, score improvement and retake facts

Strict Completion and Score Improvement are independent and default-enabled
state machines in the accepted shared design:

- Strict Completion seeks a verified completed remote state and does not start
  a retake after that state is satisfied;
- Score Improvement may create another attempt only when a fresh result exposes
  an allowed retake and the bounded score policy still requires improvement;
- the agent donor's `60` pass, `90` target and `3` attempts are evidence-backed
  example defaults, not universal Chaoxing server thresholds;
- Chapter `redoTest` and formal Exam `reTest` are different route families;
- a retake rotates QIDs and can reorder/randomize option identities, so it must
  create a fresh snapshot, remap semantic answers to current option IDs, create
  a new immutable Draft and use a new durable mutation authority;
- an ambiguous attempt-start or submit must never be replayed.

Existing task details expose score and `retake_available` as read-only facts.
They do not advertise a retake capability. No donor proves that every completed,
failed or pending-manual-grade task is retakeable, so eligibility remains an
exact result-route fact rather than a product assumption.

## Shared Core/Domain gaps reported to Main

1. Submission build currently requires exactly one selected candidate for every
   Question, receives no resolved runtime coverage policy and stores no complete
   snapshot count/unanswered identities/policy in the immutable Draft. Main
   must implement subset selection with complete-snapshot coverage binding.
2. `AnswerSource` has no independent Official/VerifiedHistorical/Unverified
   evidence state, and there is no owner-level Answer Evidence Corpus or bounded
   read-only bootstrap capability/job.
3. Retake has no explicit capability, durable attempt-creation lifecycle or
   separate Strict Completion/Score Improvement policy state.
4. `Question` has no first-class shared context/shared option/ordered child
   structure for OCS-observed type 14/15 and issue #297 shapes. Exact
   `QuestionContentFingerprint` also cannot alone authorize semantic remapping
   after option reorder.
5. BrowserBridge can open a task session but has no registered Chaoxing recipe
   lifecycle for result iframe harvesting, rendered matching/composite controls
   or browser challenge completion.

## Re-audit outcome

No donor revision changed. The sweep recovered genuine omitted scope, most
notably partial coverage, Audio, sign-in, learning-count, in-video Questions,
historical bootstrap and bounded retake/score-improvement semantics. The
coverage, corpus, retake and compound-Question work cannot be closed honestly
inside the Provider, so their precise shared gaps are recorded above. Existing
strict parsers already close the provider-private choice/true-false result
surface; unsupported result types remain fail-closed pending live-sanitized
fixtures rather than speculative parsing.

This one-time full-sweep requirement is complete for Chaoxing. Future meaningful
checkpoints return to normal revision/delta refresh while implementation
continues through the recorded shared and live-validation blockers.
