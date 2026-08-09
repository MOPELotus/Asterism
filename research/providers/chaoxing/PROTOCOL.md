# chaoxing protocol notes

These notes record donor behavior, not a verified Asterism protocol contract.
Every request must be rechecked with sanitized fixtures and a real account before
the Provider can advance beyond `Development`.

## Session and authentication

The password flow used by both Rust-port candidates is:

```text
username/password
  -> AES-CBC with `u2oh6Vu^HWe4_AES` as key and IV
  -> base64 fields
  -> POST https://passport2.chaoxing.com/fanyalogin
  -> retain response Cookie jar
  -> validate the authenticated identity/session
```

Observed validation alternatives are the SSO user endpoint and the authenticated
course-list response. A Cookie is invalid when the expected user marker is absent,
the response redirects to Passport, or the returned page is a login page.

The 2026 agent donor reports that the `uf` Cookie can be browser-fingerprint-bound.
That conflicts with assuming every imported browser Cookie will work in reqwest.
The first live test must therefore compare the same account through Native HTTP
and same-origin Browser fetch before selecting transport per capability.

## Native authentication checkpoint

The native Password transport mirrors the current donor's AES-128-CBC/PKCS#7
field encoding and bounded `fanyalogin` form. It accepts only a boolean-status
JSON response, normalizes the first pair from bounded `Set-Cookie` headers, and
requires an `_uid`/`UID` identity marker. A reported success is still provisional:
the resulting Cookie must pass the root `courselistdata` request before Core can
atomically store username, password and Cookie as a Composite credential.

ImportedCookie skips the password exchange but uses the same live course-list
validation. Known captcha and secondary/SMS messages become typed
`HumanRequired` results; Asterism does not automate either challenge. These paths
are covered only by synthetic response fixtures and donor-compatible encryption
vectors. No real credential or current live response has been recorded.

## Course identity

The web inventory posts to `mooc2-ans` course-list data and also enumerates course
folders. Stable propagation fields include `courseId`, `clazzId`, `cpi`, title,
teacher, role and course state. The mobile alternative returns the same identity
dimensions through `backclazzdata`.

Provider remote identity must include both course and class dimensions; the same
course content can appear in more than one class context.

## Course inventory checkpoint

The current offline parser consumes the structural `div.course` rows returned by
the web `courselistdata` response. Rows marked as not open are excluded. Open
rows must contain course/class identity, title, and an HTTPS
`mooc2-ans.chaoxing.com/mooc2-ans/mycourse/stu` entry whose unique
`courseid`/`clazzid` values match the row. The account-scoped `cpi` is accepted
only from that allowlisted route and is stored in non-serialized route context.

Root and folder responses are merged by `courseId + clazzId`. Identical repeats
are deduplicated; disagreeing rows fail with protocol drift rather than silently
choosing one. The Native adapter now POSTs the root and every bounded folder list
with the donor-observed form shape and interaction-page Referer while reusing one
short-lived resolved session. This remains synthetic-fixture evidence: the
requests have not been verified against a live authenticated account.

## ChapterModule

Chapter inventory and card loading propagate:

```text
courseId + clazzId + cpi
  -> Chapter tree / student course page
  -> chapterId / knowledgeId
  -> knowledge cards
  -> attachment metadata
  -> Video | Document | Read | Chapter Work
```

Chapter Work remains `ChapterModule`; it must not be merged with independent
course homework just because both ultimately use Work-shaped pages.

## WorkModule inventory

The current browser route is:

```text
GET https://mooc1.chaoxing.com/mooc2/work/list
    ?courseId={course_id}&classId={class_id}&enc={fresh_enc}
```

The `enc` value is session-bound and must be obtained from the current course Work
navigation/iframe. It is not configuration and must not be persisted as a stable
task identifier.

List text alone is insufficient:

- `未交` means never submitted, not necessarily still submittable;
- following the task URL may end at `work/dowork`, `work/prompt`, or `work/view`;
- expired/closed pages may still appear as `未交` in the list.

The scanner must preserve the remote task ID, course/class identity, title, source
time text and entry URL facts, then classify the final detail response:

| Final fact | Remote state |
|---|---|
| editor / `work/dowork` | `Pending` or `InProgress` according to saved-answer facts |
| submitted prompt / waiting for review | `Completed` |
| detail/view with expired or closed marker and no prior submission | `Expired` |
| unrecognized page | `Unknown` plus protocol-drift error evidence |

## ExamModule inventory

The current browser inventory route is:

```text
GET https://mooc1.chaoxing.com/exam-ans/mooc2/exam/exam-list
    ?courseid={course_id}&clazzid={class_id}&cpi={cpi}&ut=s
```

Unlike Work inventory, this route is reported not to require `enc`. Script blocks
must be removed before keyword classification because localized JavaScript
templates contain status words and create false positives.

Expected status facts include `待做`, `未开始`, `已完成`, `已过期`, remaining time,
score and a per-task entry action. CxKitty provides an older mobile alternative at
`/exam/phone/task-list`; it remains a fallback hypothesis until live-tested.

## Detail, submission and verification

- Exam and Work pages expose per-attempt question IDs; Exam retakes can regenerate
  every QID, so IDs may not be cached across attempts.
- Browser visual selection is not proof of a saved answer. Hidden fields and the
  server-visible result must be checked after page handlers/AJAX settle.
- HTTP 200 or a donor `status = true` response is not Asterism completion. Asterism
  must fetch the result/detail page and store a sanitized verification snapshot.
- Captcha, face verification, exam codes, minimum-submit time, expired tasks,
  access denial, and session expiry must map to distinct Provider errors or
  `HumanRequired` states rather than blind retries.

## Time handling

Donors expose a mix of absolute strings, remaining-duration text and route
parameters. Until a fixture proves timezone and format, keep the raw sanitized
source value in Provider metadata and return no normalized timestamp instead of
guessing. Parsed values must eventually preserve `opens_at`, `due_at` and
`closes_at` separately.

## Native transport checkpoint

The current Asterism adapter deliberately disables automatic redirects and
classifies Passport/login redirects as authentication failure. It builds Exam
URLs directly from ephemeral course/class/`cpi` facts. For Work it first loads
the current course page and accepts only a matching, allowlisted HTTPS iframe
route with one fresh `enc`; that value is used for the request and never enters
task identity or sanitized snapshots.

Cookie material comes from an injected Core-side resolver and response bodies
are streamed into a 4 MiB bounded, zeroizing owner. This is an offline-verified
implementation checkpoint only. No claim is made yet that the current `uf`
Cookie survives reqwest's TLS fingerprint or that the live course page exposes
the Work iframe without browser interaction.
