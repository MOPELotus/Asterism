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
    submission-prompt.html
    submission-view.html
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
token redaction, immediate Document/Read routing, Video object/report metadata,
donor-compatible signature construction, bounded playback settings,
idempotence and fresh-card result verification.
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

The three submission fixtures are donor-compatible synthetic shapes rather
than captured responses. They cover strict editor identity/type matching,
allowlisted hidden-field forwarding, removal of stale answer values, a bounded
JSON acknowledgement that remains only a Receipt, prompt-as-Pending semantics,
and exact server-visible answer comparison on the result view. They contain no
real token, answer, route or account fact and do not prove the endpoint works
against the current live platform.

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
