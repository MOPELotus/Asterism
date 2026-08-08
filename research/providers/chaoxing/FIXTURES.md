# chaoxing fixture plan

No real-account fixture was added during the 2026-08-09 static donor audit.
The repository currently contains explicitly labelled synthetic, sanitized Work
and Exam inventory/detail fixtures for parser development. They prove parser
invariants only and are not evidence of current live-platform compatibility.

## Committed synthetic fixture set

```text
fixtures/providers/chaoxing/
  work/
    list-mixed.html
    list-mixed.expected.json
    detail-editor.html
    detail-submitted.html
    detail-closed.html
  exam/
    list-empty-script-keywords.html
    list-mixed.html
    list-mixed.expected.json
```

The mixed expectation files cover normalized identity, source module and remote
state. The detail fixtures cover the conservative Work redirect classification.

## Required live-sanitized fixture sets

```text
fixtures/providers/chaoxing/
  auth/
    password-success.json
    password-rejected.json
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
