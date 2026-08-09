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

The current native inventory requests pending, unlocked Chapters with a nonzero
job count through card indexes 0 through 6. Completed or locked Chapters are not
expanded. The exact matrix is bounded and atomic; card-level `enc`, `jtoken`,
`mid`, `otherInfo`, defaults and live identifiers never enter task snapshots.

## Immediate Document and Read execution

Document and Read are the first resource execution subset. Before either call,
Asterism rediscovers the current course route and Chapter card, then matches the
stable `resource:{course}:{class}:{knowledge}:{job}` identity against exactly
one fresh attachment. Only that short-lived parsed object may expose `jtoken`
to the native request builder.

```text
Document -> GET https://mooc1.chaoxing.com/ananas/job/document
Read     -> GET https://mooc1.chaoxing.com/ananas/job/readv2
```

Both calls bind job, knowledge, course, class and `jtoken`; Document also sends
the donor-observed cache-busting timestamp. HTTP success is provisional. The
capability refetches the complete seven-card matrix and returns a verified
outcome only when the same stable attachment reports `isPassed = true`.
Already-completed attachments are idempotent and make no mutation request.
Video, Live and Chapter Work use different duration/question lifecycles and are
not routed through this immediate path.

## Video execution checkpoint

The audited primary donor obtains fresh Video metadata from the Chapter Card,
then reads the current media status before reporting progress:

```text
GET /ananas/status/{objectId}?k={fid}&flag=normal
  -> status + dtoken + duration + optional playTime

GET /mooc-ans/multimedia/log/a/{cpi}/{dtoken}
  ?clazzId&playingTime&duration&clipTime&objectId&otherInfo
  &courseId&jobid&userid&isdrag=3&view=pc&enc&dtype=Video&rt&_t
```

`enc` is the lowercase MD5 of the donor-observed ordered report tuple containing
class, user, job, object, millisecond progress, the protocol constant,
millisecond duration, and clip range. Asterism constructs this only inside the
short-lived native transport. `objectId`, `otherInfo`, `dtoken`, face-capture and
attendance fields remain redacted, zeroizing execution material and never enter
Task snapshots, logs or results.

The Core-owned Execution snapshot supplies a bounded playback rate from 1.0 to
2.0 and a 30-90 second video-progress interval. Video time advances monotonically;
the real wait for each interval is divided by the frozen playback rate. Retry
starts from freshly reported server/Card progress, not from a persisted token.
HTTP/JSON success is still provisional: the complete seven-card matrix is
re-fetched and the exact stable resource identity must report `isPassed = true`.

The donor includes automatic captcha solving. Asterism deliberately does not
port that behavior in the deferred-Capture phase: a captcha/validation response
becomes typed `HumanRequired(ImageCaptcha)` before any blind retry. The endpoint,
signature, current Referer version and real account behavior remain live-test
gates; this checkpoint is offline/native-boundary evidence only.

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

The current `TaskDetail` checkpoint does not trust a previously stored Task
snapshot. It parses the stable `module:course:class:task` identity, rediscovers
the current account course list, selects exactly one matching course through
ephemeral route context, and reruns the bounded all-or-nothing Task inventory
for that course. The exact remote Task must still exist exactly once. Work tasks
therefore include their followed final-route state; Exam tasks currently retain
the freshly parsed list state until a dedicated entry/detail fixture contract is
available.

`TaskProgressRead` preserves the same module split. Executable Resource tasks
retain the targeted fresh-card lookup used for crash recovery. Chapter, Work and
Exam tasks use exact Task rediscovery; `Completed` and `Pending` expose only
binary 100/0 completion while all other remote states leave percentage absent.
No duration or fractional score is inferred from list text.

- Exam and Work pages expose per-attempt question IDs; Exam retakes can regenerate
  every QID, so IDs may not be cached across attempts.
- The offline parser checkpoint keeps three donor modes distinct: CxKitty's
  Chapter Work `Py-mian1`/`answertype*`, its mobile Exam
  `questionWrap singleQuesId ans-cc-exam`/`questionId`, and OCS's current
  independent Work/Exam `.questionLi` preview pages. Type codes are normalized
  independently from answer resolution, hidden/current answer inputs are
  ignored, and only sanitized attachment labels are retained; fetch URLs and
  form tokens never enter the Domain Question.
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
