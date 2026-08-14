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
  exam/
    list-empty-script-keywords.html
    list-mixed.html
    list-mixed.expected.json
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
files cover normalized identity, source module and remote state. The detail
fixtures cover the conservative Work redirect classification.
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
    detail-result.html
```

Each fixture requires a sibling expectation file containing normalized remote
identity, source module, state, raw time facts and any expected classified error.

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
- Remote IDs remain stable across scans while mutable status/time facts produce a
  typed diff.
- Exam start fixtures must prove that the read-only cover freezes the exact
  course/class/exam/attempt request, that a second fresh discovery matches the
  encrypted continuation, and that only normalized Questions plus bounded
  dynamic attempt material enter the atomic artifact.
- A changed `enc_task`, `examAnswerId`, course/class route or command digest must
  fail before the one-shot start; an issued transport error must remain
  ambiguous and must never be represented by a fixture retry.
