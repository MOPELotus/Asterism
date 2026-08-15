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
| Password/Cookie/QR login, session validation, Course/folder/Chapter inventory | Authentication, CourseInventory and TaskInventory | Password/imported Cookie are implemented; QR now uses Core's encrypted CAS continuation and atomic terminal credential lifecycle, with live validation still pending |
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

Additional donor protocol behavior recovered by the sweep includes Samueli's
post-task learning-count loop, low-level normal/object/location sign-in methods, and
Video-to-Audio fallback; CxKitty's QR session, face upload and independent Exam
export; OCS's PPT/book page turns, timed Read, in-video questions and repeated
study controls; and historical `cxmooc-tools` Audio, Document, result collection
and course-wide Exam lookup. They remain explicit capability candidates rather
than being treated as out of scope. Audio is now implemented only from OCS's
explicit `insertaudio` card fact and never through failure-driven fallback.
Sign-in, learning-count manipulation, in-video Questions and Browser-only
long-tail interactions still need separate fresh fixtures and durable
mutation/verification designs before registration.

The post-sweep implementation audit further narrows those remaining claims.
Samueli's sign-in change `741d5c1` adds request helpers and an unused CLI flag,
not an end-to-end dispatcher or readback. Its learning-count change `38a2698`
increments only local state and treats missing/rejected logging calls as
non-fatal. OCS's hyperlink path binds `#hyperlink` to the surrounding frame job
but proves only a click plus a three-second wait; its timed-reader path is a
nested-frame page-turn sequence. These are valid donor protocol surfaces, but
none is verified completion. Their exact blocker/fixture criteria are recorded
in `PROTOCOL.md`, `DRIFT.md` and `FIXTURES.md`.

## Observed Question boundary

The combined donors establish these currently observed families:

| Chaoxing code/shape | Donor evidence | Current boundary |
|---|---|---|
| `0` single, `1` multiple, `3` true/false | all current donors | Parsed, native mutation and strict visible-answer verification |
| `2` fill | CxKitty, OCS, agent skill and `chaoxing-exam` | Parsed blank cardinality now gates mutation: exactly one bound blank/value is encoded for Chapter/Exam and exact single-blank visible text is fixture-covered for Chapter history/standard evidence plus Exam submitted-value verification. Multi-blank and pending/manual-grading states remain fail-closed pending exact fixtures |
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

Checkpoint closure on 2026-08-15: shared gap 1 is closed for Draft construction.
Core freezes the complete denominator, resolved minimum and exact unanswered
Question IDs. Chaoxing now consumes that partition for independent Work and
Chapter Work by re-reading a complete editor, preserving every current control
in DOM order, sending selected Draft values and rebuilding every unanswered
value as empty. The Provider continuation gap is also closed: mobile Exam v3
retains the complete ordered QID/position/content-fingerprint binding, freezes
the selected original positions, advances only selected saves and verifies
selected result evidence against the complete remote Question count. The other
shared gaps above remain independently tracked.

The same checkpoint first closed the Provider-owned portion of the recovered
CxKitty QR chain: bounded Native login/create/single-poll parsing,
Chaoxing-scoped zeroizing Cookies, exact account/AuthSession/correlation binding
and fresh Course validation. Core's encrypted restart-safe interactive-auth
continuation and atomic terminal credential lifecycle have since landed, and
the Development factory now advertises `QrCode`. Canonical continuation,
revision/sequence/result-digest and crash-recovery tests cover the integrated
boundary. In particular, a reported remote success first persists an
identity-validation continuation, so retries of the subsequent fresh Course
read cannot repeat `getauthstatus`; a sanitized live validation of the 2024
route remains open.

## Re-audit outcome

No donor revision changed. The sweep recovered genuine omitted scope, most
notably partial coverage, Audio, sign-in, learning-count, in-video Questions,
historical bootstrap and bounded retake/score-improvement semantics. The
coverage, corpus, retake and compound-Question work cannot be closed honestly
inside the Provider, so their precise shared gaps are recorded above. Existing
strict parsers now close the provider-private choice/true-false and exact
single-blank result surface; unsupported/multi-blank result types remain fail-closed pending live-sanitized
fixtures rather than speculative parsing.

This one-time full-sweep requirement is complete for Chaoxing. Future meaningful
checkpoints return to normal revision/delta refresh while implementation
continues through the recorded shared and live-validation blockers.

## 2026-08-15 sign-in supplemental sweep

The next Provider checkpoint added two current sign-in sources after the
one-time six-donor sweep. Their complete relevant default-branch surface was
re-enumerated before implementation:

- `Ylim314/chaoxing-sign` `main` at
  `7ed64ff547d352708066ff61f1b1dc1fb1be32f1` (MIT, PortSource): README and
  configuration, `server/protocol/cx/index.ts`, `cx.d.ts`, tests, exact
  `activelist`/`signDetail` routes, `type=2`, `status=1` active filtering,
  structured `Time.time`, and normal/QR/gesture/location/code dispatch;
- `superdaobo/mini-hbut` `main` at
  `64fb2f06e10c95a77a39f17d45c6e2b573ad63a2` (GPL-3.0-or-later,
  Reference): README/configuration, complete `chaoxing_checkin` module and
  tests, the same list/detail routes, numeric-or-string IDs/times, and its
  independent variant/status normalization.

This sweep found a material semantic conflict: `chaoxing-sign` treats
`status=1` as an active/doing activity and `otherId=5` as a code sign, while
`mini-hbut` treats them as signed and photo respectively. The clean-room Rust
boundary accepts only the corroborated structure. It retains status codes,
marks `otherId=5` ambiguous, binds `ifPhoto=1 + otherId=0` as the separately
observed photo shape, and exposes only course-bound list/detail reads. It does
not copy GPL source, advertise sign-in, issue a mutation or call structural
detail a completion readback.

A follow-up parameter audit compared Samueli's `741d5c1` methods with the two
new pins. Only the ordinary-sign core is sufficiently consistent for immutable
preparation: the course/activity-bound `preSign` fields and the identity-bound
normal `stuSignajax` fields are frozen into separate zeroizing templates and a
single digest. Gesture has conflicting code-validation prerequisites, location
has conflicting field sets, photo requires an uploaded `objectId`, and QR
requires dynamic `enc`; none is prepared. The preparation has no HTTP consumer
and does not weaken the durable-authority/readback gate.

The same parameter follow-up extracted only Ylim's exact response vocabulary:
`success`, `您已签到过了`, `success2`, plus pre-sign `#statuscontent` text.
Preparation-bound pure parsers now preserve those as controlled preflight or
Receipt facts with response digests; they reject all other text. No Receipt is
called completion, and no parser provides dispatch, retry or recovery.

The post-checkpoint delta audit also recovered Ylim's WebIM monitor. It obtains
three secret fields from `im.chaoxing.com/webim/me`, opens the pinned Easemob
application and treats `ext.attachment.att_chat_course.atype=2` as a sign-event
notification carrying course/class/activity identity. Asterism now covers the
bounded Native bootstrap GET and pure identity parsing only. The WebSocket
connection and durable event lifecycle are a shared Core/runtime gap. The same
audit proved that Ylim's displayed sign history is its own Prisma `SignLog`,
not a server-visible Chaoxing result and therefore not verification evidence.
