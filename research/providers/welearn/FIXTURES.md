# WELearn fixture plan

No real-account fixture was added during the 2026-08-09 static donor audit.
Initial implementation uses explicitly synthetic JSON fixtures. They prove
parser invariants only and are not live-platform evidence.

## Initial synthetic fixtures

```text
fixtures/providers/welearn/
  auth/password-success.json
  auth/password-rejected.json
  auth/password-captcha.json
  auth/password-sms.json
  courses/list-mixed.json
  courses/list-mixed.expected.json
  units/list-mixed.json
  tasks/leaves-unit-0.json
  tasks/leaves-unit-1.json
  tasks/list-mixed.expected.json
```

The Auth fixtures cover bounded response classification, strict callback
allowlisting and `HumanRequired` separation without retaining callback/token
material. The Course fixture covers numeric/string IDs, completion percentage
and unknown extra fields. Unit/SCO fixtures cover visible/hidden Units,
completed/unknown/not-open states, duration separation and stable
Course/Unit/SCO identity. Inline negative tests cover malformed rows, duplicate
identities, invalid percentages, misbound Unit responses and duration without a
completion observation.

## Required live-sanitized fixtures

```text
fixtures/providers/welearn/
  auth/password-success-live.json
  auth/password-rejected-live.json
  auth/password-captcha-live.json
  auth/password-sms-live.json
  courses/list-empty.json
  courses/list-mixed.json
  courses/course-context.html
  units/list-visible-hidden.json
  tasks/leaves-pending-completed.json
  cmi/not-attempted.json
  cmi/in-progress.json
  cmi/completed-with-duration.json
```

Before committing live-derived fixtures, remove Cookies, passwords, encrypted
passwords, OIDC/PKCE state, `uid`, `classid`, real IDs, names, institutions,
scores, free-form answers, device identifiers and request identifiers. Preserve
only structural field names, response codes and bounded placeholder shapes.

## Regression requirements

- String and numeric `cid`/SCO IDs normalize identically.
- Unknown/missing completion text maps to `Unknown`, never `Pending` by default.
- Hidden Units/SCOs remain visible as `NotOpen` observations rather than being
  silently deleted.
- `iscomplete` alone never supplies duration.
- `learntime` alone never marks a task completed.
- Unknown fields are dropped from sanitized normalized data.
