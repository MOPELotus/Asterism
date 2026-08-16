# chaoxing fixture plan

No real-account fixture was added during the 2026-08-09 static donor audit.
The repository currently contains explicitly labelled synthetic, sanitized
Course, sign-in, Work and Exam inventory/detail fixtures for parser development. They
prove parser invariants only and are not evidence of current live-platform
compatibility.

## Committed synthetic fixture set

```text
fixtures/providers/chaoxing/
  auth/
    password-success.json
    password-rejected.json
    password-captcha.json
    password-sms.json
    qr-login.html
    qr-awaiting.json
    qr-scanned.json
    qr-authenticated.json
    qr-rejected.json
    qr-expired.json
  courses/
    interaction-folders.html
    list-mixed.html
    list-mixed.expected.json
  sign/
    activities-mixed.json
    detail-normal.json
    pre-sign-no-completion.html
    pre-sign-completed.html
    receipt-accepted.txt
    receipt-already-signed.txt
    receipt-window-closed.txt
    webim-me.html
    webim-sign-event.json
    webim-non-sign-event.json
  work/
    course-page-with-work-iframe.html
    list-mixed.html
    list-mixed.expected.json
    detail-editor.html
    detail-submitted.html
    detail-closed.html
    submission-editor.html
    submission-editor-fill.html
    submission-editor-partial.html
    submission-prompt.html
    submission-view.html
    submission-view-fill.html
    submission-view-partial.html
    chapter-submission-editor.html
    chapter-result.html
    chapter-history-result.html
    chapter-history-result-fill.html
  exam/
    list-empty-script-keywords.html
    list-mixed.html
    list-mixed.expected.json
    detail-result.html
    detail-result-partial.html
    detail-result-fill.html
    cover-ready.html
    start-question.html
    submit-save-1.json
    submit-save-2.json
    submit-final.json
    submit-rejected.json
  questions/
    work-mobile-mixed.html
    exam-mobile-mixed.html
    exam-mobile-fill.html
    work-preview-mixed.html
    exam-preview-mixed.html
  chapter/
    list-mixed.html
    list-mixed.expected.json
  resources/
    cards-mixed.html
    cards-mixed.expected.json
```

The Password Auth fixtures cover bounded JSON outcome classification only;
synthetic `Set-Cookie` headers remain in unit tests. The QR fixtures cover one
strict login-page `uuid`/`enc` pair and the donor-observed awaiting, scanned,
authenticated, rejected and expired JSON states. Nickname, UID and message text
use deliberate `PRIVATE_*` sentinels which tests never retain. Separate Cookie
tests prove Chaoxing-only domain/path scope, host-only isolation, replacement,
deletion and zeroized header assembly. Provider tests additionally encode and
decode the canonical continuation, reject foreign account/session/correlation,
phase/digest/revision/sequence drift, accept a monotonic sequence gap left by a
released retryable Core claim while rejecting stale reuse, rotate scan to
confirmation and then to durable identity validation, persist an authenticated
terminal only after the separate read-only validation step, deterministically
finalize the same Cookie bundle, and classify rejected/expired terminal
outcomes. Core repository/workflow tests cover encrypted CAS persistence
separately. No fixture claims live QR compatibility.

The Course fixtures cover bounded
folder discovery, class-scoped identity,
unopened-course filtering, bounded metadata, ephemeral `cpi`, and strict
allowlisting of the current course entry route. The Work course-page fixture
covers strict discovery of a fresh Work iframe route. The mixed expectation
files cover normalized identity, source module and remote state. The mixed Exam
fixture additionally covers an optional bounded decimal score, structural
`reTest(...)` availability, and minute text that must not become a score. The
Exam result fixture covers the same facts after a strictly bound fresh preview
read, while the Work detail fixtures cover conservative redirect classification.
The Chapter and Resource fixtures cover stable knowledge/job identity, locked
and completed state, the exact resource type split, empty card slots, execution
token redaction, immediate Document/Read routing, structurally distinct
`insertvideo`/`insertaudio` media with separate object/report metadata,
donor-compatible signature/Referer/`dtype` construction, bounded playback
settings, and Live route identity redaction plus ordered heartbeat issue/receipt
sequencing.
Resource mutation receipts remain provisional until fresh-card result
verification.
The Question fixtures separate CxKitty's mobile Chapter Work and Exam structures
from the current OCS independent `/mooc2/work/dowork` and
`/mooc2/exam/preview` structures. Attempt-local IDs, type inputs, stems, options
blank cardinality and non-fetchable attachment labels are retained, while current/hidden answer
inputs and every submission field are deliberately excluded from normalized
output. The legacy `work-mobile-mixed.html` filename refers specifically to
Chapter Work and is not an independent Work transport fixture. The independent
Work preview fixture also exercises the registered native Question reader after
strict route acquisition. The Chapter list, seven-slot Resource matrix and
legacy `work-mobile-mixed.html` fixture now exercise the Chapter Work reader's
fresh job/token rebinding, single attempt acquisition and page-kind-bound cache.
They remain synthetic parser/transport-boundary evidence rather than live
responses.

The submission fixtures are donor-compatible synthetic shapes rather
than captured responses. They cover strict editor identity/type matching,
allowlisted hidden-field forwarding, removal of stale answer values, a bounded
JSON acknowledgement that remains only a Receipt, prompt-as-Pending semantics,
and exact server-visible answer comparison on the result view. They contain no
real token, answer, route or account fact and do not prove the endpoint works
against the current live platform.

`submission-editor-fill.html` and `submission-view-fill.html` pin the exact
single-blank independent Work subset. The complete two-Question editor keeps an
unselected choice empty, discards a stale fill value, retains type code 2 and
sends one exact Draft text through `answer{qid}`. The result fixture confirms or
rejects only the selected fill at its original position from one bounded visible
`我的答案` value; a missing value is Inconclusive. Tests reject `blank_count=2`
and a forged independent short-answer type. These fixtures are synthetic donor
contract evidence, not live teacher-policy validation.

`submission-editor-partial.html` and `submission-view-partial.html` cover the
Core-frozen subset path. Four fresh controls include two selected Questions, one
unanswered supported text Question and one unanswered observed matching type.
Tests require the complete count and DOM order, rebuild both unanswered values
as empty, retain their numeric type fields, discard all stale values, and verify
only the two selected answers against their original positions. Missing a
Question makes form preparation fail as `RemoteChanged` and result verification
Inconclusive; a selected mismatch rejects only that selected Question. The
fixtures remain synthetic and do not prove that a live teacher policy accepts
the same partial final submission.

`work/chapter-submission-editor.html` adds the primary donor's synthetic
Chapter Work `form1` shape. Tests prove stale answer inputs and unknown fields
are discarded, type codes and one Question-bound single-blank text are encoded
from the immutable Draft, ambiguous multi-blank text fails closed, the mutation
occurs once, and a separate completed-card read confirms only task-level
completion.

`work/chapter-result.html` models the 2026-06 Chapter-test donor's completed
`selectWorkQuestionYiPiYue` iframe with `singleQuesId[data=qid]`, localized
Question types, visible `我的答案` / `正确答案`, and `最终成绩`. Parser tests prove
complete ID/order/type/value binding for single-choice, multiple-choice and
true/false, per-Question rejection rather than all-or-nothing status, exact
fixed-point scores for 0, 82.5 and 99.999, and `None` when score is absent.
Missing/extra/reordered/multi-blank or pending-text facts stay Inconclusive;
duplicate IDs and malformed, out-of-range or conflicting scores fail closed. The fixture is
synthetic parser evidence and does not claim that Native HTTP can reach the
iframe result route.

The same fixture's `正确答案` fields exercise Provider-native candidate mapping.
Tests require the complete QID/order/type set and current option IDs, producing
exact single-choice, multiple-choice and true/false candidates with sanitized
provenance. Missing standard answers, reordered Questions and unknown options
fail closed. No test advertises `AnswerResolve`, clicks `redoTest` or treats the
synthetic iframe as live route evidence.

`work/chapter-history-result.html` is a separate synthetic read-only history
fixture. It contains one matching and two differing submitted/official answer
pairs, an exact fixed-point score and a structural `redoTest(...)` entry. The
Provider-local result-evidence parser keeps `我的答案`, `正确答案`, their
per-Question equality judgement, score and retake entry as distinct facts; its
`Debug` output redacts answer values. Missing either answer label fails closed,
and a lookalike `notRedoTest(...)` handler does not create retake availability.
The fixture neither grants mutation authority nor proves BrowserBridge access
to a live historical result.

`work/chapter-history-result-fill.html` adds a Question-bound type-2 single
blank with exact visible submitted and official text. Tests retain those values
as separate redacted evidence, produce one ProviderNative standard candidate,
confirm an exact submitted value even when only the official value is pending,
and stay Inconclusive when the submitted value itself is pending. Rebinding the
same result to `blank_count=2` is unsupported; no concatenation or inferred
blank partition is permitted.

`work/chapter-history-result.html` also contains the minimum result-page stem/type/current-option
DOM needed to reconstruct Questions under the Core-provided TaskId. A synthetic
two-page `ChaoxingAnswerHistoryTransport` proves sanitized ordinal cursors,
strict `workAnswerId`-based Provider attempt digests, an exact result-document
digest, Task-bound Questions and separate submitted/official/correctness/score/
retake output. Invalid cursor zero, a changed attempt digest and lookalike
retake handlers fail before the result transport. No synthetic route is used by
the native development factory.

`questions/exam-mobile-fill.html` and `exam/detail-result-fill.html` form the
matching mobile Exam contract. The Question parser binds exactly one rendered
blank, the save body emits only `answer{qid}1` plus `blankNum{qid}=1,`, and the
result verifier confirms/rejects only one exact bounded visible text value.
Pending-label values are Inconclusive and changing the bound blank count makes
mutation fail closed. These are synthetic shapes, not live route validation.

Capability-level tests wrap the same bounded fixture with a fake fresh
TaskDetail and result transport. They prove that the typed resolver emits three
bound ProviderNative candidates only after exact completed
course/class/knowledge/job rediscovery, and that pending Task state or a foreign
Question page kind fails before the result transport is called. The development
factory remains unregistered and no fixture represents external Tiku, searcher,
answer-wrapper, model or random output as Provider-native evidence.

The Exam fixtures cover the cover/start/full-preview attempt rotation and the
separate CxKitty save/final acknowledgement shapes. They prove that each
accepted answer save advances exactly one selected immutable-Draft cursor,
uses the selected Question's original zero-based full-paper position, and replaces
only monotonic timing/`enc` state, while the final response remains a Receipt
until a fresh exact Exam list row reports Completed. `detail-result.html` adds
two ordered synthetic result Questions with exact IDs, type 0/3 inputs and
visible `我的答案` values. Tests prove exact confirmation, value rejection and
Inconclusive handling for missing, extra, unsupported or drifted binding facts;
duplicate IDs fail closed. The synthetic result score additionally proves exact
fixed-point projection for 0, 82.5 and 99.999 plus `None` when absent, without
changing answer status. Unknown compatible JSON extensions are ignored, but
malformed state, rejection, identity drift and timing regression fail closed.
All IDs, signatures and crypto-looking values are synthetic placeholders.

`detail-result-partial.html` retains four complete result identities while only
the second original Question is selected. It proves that v3 continuation
binding sends one save with `start=1`, reaches final submission after one
selected save, and verifies that selected visible answer independently. The
unselected type 11/15 Questions and absent visible values remain structural
full-paper evidence and are never treated as submitted answers.

## Required live-sanitized fixture sets

```text
fixtures/providers/chaoxing/
  auth/
    session-expired.html
  courses/
    root-and-folders.html
  work/
    list-empty.html
    list-pending-submittable.html
    list-pending-expired.html
    list-submitted.html
    detail-editor.expected.json
    detail-submitted.expected.json
    detail-closed.expected.json
  exam/
    list-empty-script-keywords.html
    list-pending.html
    list-not-open.html
    list-completed.html
    list-expired.html
    detail-result.live.html
    detail-result.live.expected.json
  harvest/
    completed-chapter-work.live.html
    completed-chapter-work.live.expected.json
    completed-work.live.html
    completed-work.live.expected.json
    completed-exam.live.html
    completed-exam.live.expected.json
    fill-reference-pending.live.html
    fill-reference-pending.live.expected.json
    matching-shared-context.live.html
    matching-shared-context.live.expected.json
```

Each fixture requires a sibling expectation file containing normalized remote
identity, source module, state, raw time facts and any expected classified error.
The committed `detail-result.html` remains explicitly synthetic; a future live
sanitized capture must use the `.live` sibling and must not overwrite it.

## Sanitization requirements

Before committing a fixture, replace or remove:

- all Cookie, Authorization, Passport, captcha and face-verification values;
- `_uid`, `UID`, `uf`, `fid`, `puid`, `cpi`, `enc`, `enc_task`, `ktoken` and tokens;
- real course, class, task, answer, question and user IDs;
- names, phone numbers, schools, student IDs, titles that reveal enrollment, scores
  and free-form answers;
- request IDs, device IDs, browser fingerprints and upload/object identifiers.

Keep structural field names, element hierarchy, status labels, response codes,
redirect shape and bounded placeholder lengths. Run the repository secret scan on
every fixture before staging it.

## Regression requirements

- Script-template status words must not create phantom Exams.
- `未交` plus a closed detail page maps to `Expired`, not `Pending`.
- Chapter Work, independent Work and Exam receive distinct source types even when
  their question markup is identical.
- Unknown status text is retained as sanitized evidence and maps to `Unknown`.
- Exam score accepts only 0-100 with at most three fractional digits; malformed,
  out-of-range or conflicting evidence fails closed, and `分钟` is never a score.
- Exam retake availability requires an exact row-local `reTest(...)` handler;
  lookalike handlers stay false and the fact never changes state or capability.
- Supported Exam result routes must bind the exact course/class/exam identity;
  missing, duplicate, foreign, unexpected or list-conflicting detail facts fail
  the complete scan, and redirects are not accepted as result evidence.
- Remote IDs remain stable across scans while mutable status/time facts produce a
  typed diff.
- Audio regression must require an exact fresh `property.module=insertaudio`,
  use the Audio Referer and `dtype=Audio`, retain the same signed object/job
  binding, and verify completion from a separate fresh card read. No fixture may
  authorize switching from Video to Audio after an ambiguous request failure.
- Exam start fixtures must prove that the read-only cover freezes the exact
  course/class/exam/attempt request, that a second fresh discovery matches the
  encrypted continuation, and that only normalized Questions plus bounded
  dynamic attempt material enter the atomic artifact.
- A changed `enc_task`, `examAnswerId`, course/class route or command digest must
  fail before the one-shot start; an issued transport error must remain
  ambiguous and must never be represented by a fixture retry.
- Exam preview state supersedes start-page state. Every answer-save operation
  must have a distinct persisted request digest; only an accepted response may
  rotate the continuation. Temporary-save ambiguity is never replayed, final
  ambiguity needs fresh Completed evidence, and task completion/score does not
  imply per-Question answer verification. Exact result confirmation requires
  the complete v3-bound remote QID/order set plus each selected Draft Question's exact
  original position/type/value; missing, extra or drifted selected evidence is
  Inconclusive, duplicates fail closed, and hidden inputs/CSS are ignored.
  Unsupported unselected numeric types and absent unselected visible answers
  remain non-answer evidence. Optional result scores retain exact thousandths on every answer
  status and never become answer-consistency evidence.
- Chapter Work result confirmation requires a fresh, bound
  `selectWorkQuestionYiPiYue` iframe document in addition to the completed card.
  The synthetic parser fixture does not satisfy the pending BrowserBridge/live
  route validation requirement.
- Chapter Work standard-answer evidence must bind to the current Question option
  IDs. A retake's randomized DOM `data` mapping invalidates any earlier mapping;
  parser candidates do not authorize or execute the retake.
- Historical harvest fixtures must prove that discovery and result reads do not
  create an attempt. Their expectations keep completion, score, submitted
  answer, official/reference answer, correctness and retake availability in
  distinct fields and bind every fact to owner/account/course/task/attempt.
- Fill/short fixtures must preserve exact standard/reference/pending-manual-grade
  labels. Until those live shapes exist, result harvesting remains unsupported
  rather than parsing nearby score or generic answer text.
- Matching, cloze, reading and shared-option fixtures must retain shared context,
  option identities and ordered child grading. A flat synthetic string is not a
  valid substitute for the observed structure.
- Coverage regression must include a complete snapshot containing an unanswered
  supported Question, an unsupported observed Question and an unknown type; the
  denominator is the complete snapshot and no missing value is invented.
- Independent Work fill regression requires `provider_type_code=2`, a
  page-derived `blank_count=1`, one Draft text, one fresh `answer{qid}` field and
  one exact visible result value. Multi-blank concatenation and independent
  short-answer mutation remain unsupported rather than borrowing Chapter Work
  semantics.
- Retake fixtures must show fresh QIDs/option mappings and separate Chapter
  `redoTest` from Exam `reTest`; they may not authorize mutation during the
  read-only bootstrap test path.
- Sign-in now has synthetic exact active-list and separately bound
  `signDetail` fixtures. They cover mixed sign/non-sign rows, numeric/string
  identities, direct/nested times, photo refinement, the non-conflicting
  normal/QR/gesture/location mappings, explicit `otherId=5` ambiguity, raw
  status retention, duplicate rejection and course/detail drift. Remaining
  fixtures must include `preSign`, required object/location inputs, opaque
  mutation responses and a genuinely independent account sign-result readback.
  `signDetail` alone is not completion evidence, and an opaque `stuSignajax`
  string is a Receipt at most and never authorizes replay after ambiguity.
  Provider unit regressions additionally derive one ordinary-sign preparation
  from these fixtures, assert its exact two query field sets and actor-bound
  digest, redact UID/name, and reject foreign snapshots, non-normal variants
  and malformed actor inputs without issuing HTTP.
  Separate bounded documents bind both pre-sign pages and all three exact
  donor-observed response strings to that preparation digest. Regressions reject
  missing/duplicate/unknown `#statuscontent`, unknown opaque mutation text and
  oversized bodies, while keeping preflight evidence, mutation Receipt and
  independent completion verification as three distinct concepts.
  A future transport fixture must additionally ledger `preSign` as the first
  issued operation in the sign mutation sequence, because the PortSource says
  that request is required for the later sign record. GET method semantics must
  not bypass issue persistence, and an ambiguous pre-sign outcome must not be
  replayed without a separately evidenced recovery rule.
  WebIM fixtures separately cover the exact three bootstrap selectors, one
  `atype=2` `att_chat_course` identity and one non-sign activity. Tests bind
  credentials to account/correlation without exposing token/UID/name, retain
  only course/class/activity IDs from a sign event, ignore non-sign events and
  reject malformed sign identities. They do not simulate a WebSocket session,
  reconnect, acknowledgement, mutation or completion readback. Future
  subscription fixtures must carry an explicit event identity/cursor and
  persisted lease/recovery state; the donor's callback shape supplies none, so
  a message digest alone cannot stand in for ordered de-duplication.
- Learning-count fixtures must expose a fresh server-visible count before and
  after one visit. `studentstudyAjax`, `setlog` and monitor responses alone are
  insufficient, and a synthetic local target counter is not verification.
- Hyperlink/timed-reader fixtures must retain nested-frame identity, the exact
  `#hyperlink` or `bookifame` selector family, attachment/job binding and a
  separate fresh completed-card observation. A successful click, page turn or
  elapsed timer is not completion evidence.
- In-video Question fixtures must retain exact media/Question/option identity
  and a server-visible saved result. Random selection is never a valid fixture
  expectation.
- Completion-diagnosis regression maps only a fresh Expired verification to
  `WindowClosed`. Pending, InProgress, NotOpen, Removed, Unknown and Completed
  must not be guessed as prerequisite, teacher review, duration or score facts.
- Protocol-observation regressions cover unknown numeric Question type,
  unknown Chapter resource structure and unknown Work result shape. Deliberate
  `PRIVATE_*` raw values must remain absent from serialized observations; only
  controlled page/module classes, counts and known-field presence may cross the
  boundary.
