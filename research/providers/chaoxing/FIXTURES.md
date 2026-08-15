# chaoxing fixture plan

No real-account fixture was added during the 2026-08-09 static donor audit.
The repository currently contains explicitly labelled synthetic, sanitized
Course, Work and Exam inventory/detail fixtures for parser development. They
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
  courses/
    interaction-folders.html
    list-mixed.html
    list-mixed.expected.json
  work/
    course-page-with-work-iframe.html
    list-mixed.html
    list-mixed.expected.json
    detail-editor.html
    detail-submitted.html
    detail-closed.html
    submission-editor.html
    submission-editor-partial.html
    submission-prompt.html
    submission-view.html
    submission-view-partial.html
    chapter-submission-editor.html
    chapter-result.html
  exam/
    list-empty-script-keywords.html
    list-mixed.html
    list-mixed.expected.json
    detail-result.html
    cover-ready.html
    start-question.html
    submit-save-1.json
    submit-save-2.json
    submit-final.json
    submit-rejected.json
  questions/
    work-mobile-mixed.html
    exam-mobile-mixed.html
    work-preview-mixed.html
    exam-preview-mixed.html
  chapter/
    list-mixed.html
    list-mixed.expected.json
  resources/
    cards-mixed.html
    cards-mixed.expected.json
```

The Auth fixtures cover bounded JSON outcome classification only; synthetic
`Set-Cookie` headers remain in unit tests. The Course fixtures cover bounded
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
and non-fetchable attachment labels are retained, while current/hidden answer
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
are discarded, type codes and ordered fill text are encoded from the immutable
Draft, the mutation occurs once, and a separate completed-card read confirms
only task-level completion.

`work/chapter-result.html` models the 2026-06 Chapter-test donor's completed
`selectWorkQuestionYiPiYue` iframe with `singleQuesId[data=qid]`, localized
Question types, visible `我的答案` / `正确答案`, and `最终成绩`. Parser tests prove
complete ID/order/type/value binding for single-choice, multiple-choice and
true/false, per-Question rejection rather than all-or-nothing status, exact
fixed-point scores for 0, 82.5 and 99.999, and `None` when score is absent.
Missing/extra/reordered/fill-blank facts stay Inconclusive; duplicate IDs and
malformed, out-of-range or conflicting scores fail closed. The fixture is
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

The same fixture now contains the minimum result-page stem/type/current-option
DOM needed to reconstruct Questions under the Core-provided TaskId. A synthetic
two-page `ChaoxingAnswerHistoryTransport` proves sanitized ordinal cursors,
strict `workAnswerId`-based Provider attempt digests, an exact result-document
digest, Task-bound Questions and separate submitted/official/correctness/score/
retake output. Invalid cursor zero, a changed attempt digest and lookalike
retake handlers fail before the result transport. No synthetic route is used by
the native development factory.

Capability-level tests wrap the same bounded fixture with a fake fresh
TaskDetail and result transport. They prove that the typed resolver emits three
bound ProviderNative candidates only after exact completed
course/class/knowledge/job rediscovery, and that pending Task state or a foreign
Question page kind fails before the result transport is called. The development
factory remains unregistered and no fixture represents external Tiku, searcher,
answer-wrapper, model or random output as Provider-native evidence.

The Exam fixtures cover the cover/start/full-preview attempt rotation and the
separate CxKitty save/final acknowledgement shapes. They prove that each
accepted answer save advances exactly one immutable-Draft cursor and replaces
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
  the immutable Draft's full ID/order/type/value set; missing, extra or type-2
  evidence is Inconclusive, duplicates fail closed, and hidden inputs/CSS are
  ignored. Optional result scores retain exact thousandths on every answer
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
- Retake fixtures must show fresh QIDs/option mappings and separate Chapter
  `redoTest` from Exam `reTest`; they may not authorize mutation during the
  read-only bootstrap test path.
- Completion-diagnosis regression maps only a fresh Expired verification to
  `WindowClosed`. Pending, InProgress, NotOpen, Removed, Unknown and Completed
  must not be guessed as prerequisite, teacher review, duration or score facts.
- Protocol-observation regressions cover unknown numeric Question type,
  unknown Chapter resource structure and unknown Work result shape. Deliberate
  `PRIVATE_*` raw values must remain absent from serialized observations; only
  controlled page/module classes, counts and known-field presence may cross the
  boundary.
