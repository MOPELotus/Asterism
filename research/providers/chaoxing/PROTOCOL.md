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

The donor includes automatic captcha solving. The current Native HTTP boundary
turns a captcha/validation response into typed `HumanRequired(ImageCaptcha)`
before any blind retry; the donor solver and a bounded Capture/BrowserBridge
handoff are active first-batch work. The endpoint, signature, current Referer
version and real account behavior remain live-test gates; this checkpoint is
offline/native-boundary evidence only.

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

The bounded Exam row parser retains a missing score as `null` or a decimal
`score` in the inclusive 0-100 range with at most three fractional digits.
Structural score fields and contextual `成绩`/`得分` text are accepted;
minute-duration text is excluded and conflicting or malformed score facts fail
closed. `retake_available` is true only when that exact row contains a
structural `reTest(...)` handler. Both values are fresh read-only facts: they do
not change `RemoteState`, advertise a mutation capability, or execute a retake.

## Detail, submission and verification

The current `TaskDetail` checkpoint does not trust a previously stored Task
snapshot. It parses the stable `module:course:class:task` identity, rediscovers
the current account course list, selects exactly one matching course through
ephemeral route context, and reruns the bounded all-or-nothing Task inventory
for that course. The exact remote Task must still exist exactly once. Work tasks
therefore include their followed final-route state. A completed Exam row with a
structural `/exam-ans/mooc2/exam/preview` entry now receives one fresh result
read. The HTTPS route must bind the same course, class and Exam identity with no
duplicate query keys, userinfo, port or fragment; redirects are not followed.
The bounded result parser removes script/style content and accepts only score,
structural `reTest(...)`, or answer-result evidence. It merges score and retake
facts into the fresh Task, records separate `detail_score` and
`detail_retake_available` provenance, and fails the entire scan if list and
detail scores conflict or the transport omits, duplicates or adds a Task.
Unsupported Exam entry variants remain list-level evidence pending fixtures.

`TaskProgressRead` preserves the same module split. Executable Resource tasks
retain the targeted fresh-card lookup used for crash recovery. Chapter, Work and
Exam tasks use exact Task rediscovery; `Completed` and `Pending` expose only
binary 100/0 completion while all other remote states leave percentage absent.
No duration or fractional completion is inferred from the independent score
fact. Result readback never changes `RemoteState` or capabilities and never
executes a retake.

## Native independent Work question read

Independent Work question inventory does not reuse a stored task entry or
session-bound `enc`. It rediscovers the current course, obtains a fresh Work
inventory entry, follows only the existing fixed-host Work redirect allowlist,
and accepts the response as a readable question page only when the final path
ends in `/work/dowork`. Final `work/view`, `work/prompt`, login and unknown pages
fail closed instead of producing a partial question list.

The bounded page is parsed once and retained only as normalized Questions in a
five-minute, process-local cache keyed by ProviderAccount, correlation ID and
stable Work task identity. `QuestionInventory` returns references from that
attempt and `QuestionParse` must consume matching references in the same Core
read. The cache has a hard capacity, is removed after the final Question, and
never persists HTML, entry URLs, `enc`, form tokens or attempt-local QIDs.

## Native Chapter Work question read

Chapter Work remains a `resource:course:class:knowledge:job` task. A read first
rediscovers the exact Course and Chapter, fetches the complete card indexes
`0..=6`, rejects foreign, missing or duplicate cards, and locates exactly one
pending `workid` attachment. `jobid`, `enc` and the card-root `ktoken` are held
only in a redacted, zeroizing operation target.

The primary donor then performs one GET to
`https://mooc1.chaoxing.com/mooc-ans/api/work` with the exact
course/class/knowledge/cpi, `workId`, `jobid`, `originJobId`, `enc` and `ktoken`
binding. This GET can create an attempt. Asterism may renew authentication only
before sending it and never replays an ambiguous response. Redirects are
manually bounded to the same host and the audited `doHomeWorkNew` route;
`clazzId` on the initial request and `classId` on the redirect are treated as
exclusive aliases with the same required value. The server-generated final
`workId` is accepted only alongside the exact `oldWorkId`, job and route scope.

The bounded mobile page is parsed through the existing Chapter Work grammar and
stored only as normalized Questions in the same account/correlation/task-bound
short-lived cache. Page-kind binding prevents an independent Work reference
from being consumed as Chapter Work.

`SubmissionBuild` keeps selected values out of the preview and maps donor type
codes 0/1/2/3/4 to single-choice, multiple-choice, fill-blank, true/false and
short-answer. Ordered fill texts are concatenated exactly as the donor does;
short-answer accepts one text value and other shapes fail closed.

`SubmissionExecute` rebuilds from the immutable Draft, rebinds the exact
Course/Chapter/seven-card target, acquires one fresh editor and forwards only
audited form fields before one `addStudentWorkNew` POST. Neither attempt GET nor
POST is replayed after an ambiguous send, and the JSON success flag is only a
Receipt. Independent `SubmissionVerify` refetches seven cards and confirms only
when the exact attachment is Completed. Per-Question facts stay Unverified when
the card exposes no answer readback. Answer resolution and live validation
remain active work; Exam keeps its separate attempt/gate/verification family.

The 2026-06 Chapter-test donor separately records completed/waiting results in a
`selectWorkQuestionYiPiYue` iframe. Its current DOM keeps each Question in
`div.singleQuesId[data=qid]`, exposes a localized Question type and visible
`我的答案` / `正确答案`, and reports `最终成绩`. The Provider-side result parser
requires a complete unique QID set in immutable-Draft order, exact type and
visible answer values for single-choice, multiple-choice and true/false. It
emits per-Question Confirmed/Rejected facts and retains the independent 0-100
score as fixed thousandths on any verification status. Missing, extra,
reordered, fill/short-answer or unsupported evidence remains Inconclusive;
duplicates and malformed or conflicting scores fail closed. The parser is an
offline boundary only until Main's BrowserBridge can supply the fresh bound
study-page iframe document. The completed card alone continues to produce only
task-level confirmation.

The same strict result boundary parses `正确答案` independently from the user's
submitted value. When every result Question matches the current Question set by
QID, DOM order and type, single-choice, multiple-choice and true/false standard
values become full-confidence `ProviderNative` AnswerCandidates. Each choice
must still name an option ID in the fresh Question snapshot; the candidate
provenance records only the result-route schema and remote QID. Missing standard
answers, fill/short-answer types, reordered Questions or unknown options fail
closed. This pure parser neither advertises `AnswerResolve` nor invokes
`redoTest`; both require the Main-owned BrowserBridge binding and a separately
durable retake operation.

The Provider now wraps that pure parser in an unregistered typed
`ChaoxingAnswerResolve`. The capability validates the authenticated Provider
context and a complete same-Task, consecutive `chapter_work_mobile` Question
snapshot, refreshes TaskDetail, and accepts only an exact completed
`resource:course:class:knowledge:job` whose normalized facts match every stable
component. Its transport receives a credential-free
`ChaoxingChapterWorkResultRequest` and must return one bounded, zeroizing result
document; candidate parsing then repeats QID/order/type/current-option binding.
No development metadata, factory entry or Task advertises `AnswerResolve` yet.
The blocker is architectural rather than parser uncertainty: the standard is
post-result evidence reached through a bound Browser iframe, whereas the normal
Core resolution phase precedes submission. Main must provide the iframe read and
an explicit lifecycle before registration. No pending independent Work, pending
Chapter Work or Exam donor exposes a Chaoxing-native standard-answer endpoint.
External Tiku/searcher/wrapper/model results and random/fuzzer guesses are never
reported as `ProviderNative`.

CxKitty's QR path is independently actionable after this checkpoint. One Web
Cookie jar loads `passport2.chaoxing.com/login`, extracts bounded `uuid`/`enc`,
activates `createqr?uuid=...&fid=-1`, presents the exact
`toauthlogin?uuid=...&enc=...` payload, and polls `getauthstatus` with the same
two values. A clean-room Provider command can own parsing and polling semantics,
but shared challenge presentation and atomic owner-bound credential commit are
required before the authentication method can be advertised.

## Native independent Work submission

The primary donor names every answer field `answer{qid}` and every type field
`answertype{qid}`, builds `answerwqbid`, sets an empty `pyFlag` for final submit,
and POSTs `https://mooc1.chaoxing.com/mooc-ans/work/addStudentWorkNew`. The older
mobile donor confirms the same answer/type grammar but exposes a different
global parameter set. Asterism therefore does not persist or blindly copy an
executable donor payload.

`SubmissionBuild` remains credential-free and value-free. It accepts only
single-choice, multiple-choice and true/false Questions whose remote option
identities have the unambiguous donor grammar. It validates the complete
Question/SelectedAnswer binding and returns only the two expected field names
per Question. Fill-blank, short-answer and complex shapes fail closed until a
current live fixture proves their exact multi-blank/editor encoding.

`SubmissionExecute` reloads that immutable Draft, rebuilds the expected preview,
rediscovers the current Course and Work inventory, and follows the fresh Work
route to an allowlisted `/work/dowork` editor. The editor must expose the exact
same QID/type set, matching course/class identity, question count and required
attempt material. Only a fixed audited set of hidden fields is forwarded;
unknown fields and all existing `answer*` values are discarded before the
Draft answers, `answerwqbid` and final-submit `pyFlag` are added.

The POST occurs once. Authentication may be renewed only while the preceding
editor GET is known to have failed; no error after the mutation request causes a
second POST. A bounded `{status:true}` JSON response becomes an `accepted`
`SubmissionReceipt` with no route token or answer data. It is never completion.
Captcha and face/browser challenges are typed `HumanRequired`; expiry and
deletion become `RemoteChanged`; malformed or unknown responses fail closed.

`SubmissionVerify` is an independent read-only slot and does not need a Receipt,
which lets Core recover an ambiguous submit. It rediscovers the same Course and
Work and follows the current detail route. `/work/dowork` is Pending. A
`/work/prompt` page with submission/waiting evidence is also Pending because it
does not expose the submitted values. Only an allowlisted `/work/view` result
whose complete unique QID/type set exposes `我的答案` and exactly matches every
Draft answer becomes Confirmed with remote Completed. A complete mismatch is
Rejected; incomplete result facts are Inconclusive. CSS selection state, list
text, HTTP 200 and the acknowledgement JSON never substitute for this readback.

These contracts are covered by explicitly synthetic donor-compatible fixtures
and capability-level tests, including recovery verification with no Receipt.
They are native/offline evidence only; the Provider remains Development and no
live submission claim is made before the planned WebUI/Asterism-Plugin-assisted
account validation.

Exam is not given Work's payload semantics. In the audited mobile donor, the
readable attempt-local question/preview chain follows the mutating exam-start
route and requires the resulting active attempt identity and `enc`; that path
can also encounter exam-code, face or captcha gates. Asterism implements this
as a separate durable capability family. The start or submit receipt is not
proof of completion, and an ambiguous non-idempotent start/save/submit is never
blindly replayed.

The first native Exam Question slice now follows that boundary through Core's
durable pre-Question attempt ledger. A fresh `exam:course:class:exam` task must
resolve to one current Exam list row with a structural `goTest` entry and
bounded `enc_task`. The Provider obtains the cover, rejects completed/not-started
states, and encodes an owner/account/Task-bound `chaoxing.exam-pre-question.v1`
continuation. Before execution it repeats Course/Exam/cover discovery and
requires the exact encrypted continuation digest to match.

Core persists `chaoxing.exam-start.v1` and the canonical request digest before
the Provider performs one `phone/start`. The Native transport accepts only the
bound redirect and returned `examAnswerId`, then reads the complete
dynamic-`enc` mobile preview. The preview's newer `enc`, remaining-time and
last-update values supersede the start page. Success atomically materializes
normalized Questions plus the minimal encrypted
`chaoxing.exam-question-attempt.v2` artifact containing route, attempt/timing
material, Question count, ordered Question-set fingerprint and next-save
cursor. No HTML or answer content enters the artifact. Any error after issue
remains ambiguous and cannot be auto-replayed. Cover or start responses
requiring an exam code, face check or captcha return `HumanRequired` for
BrowserBridge/Capture handoff.

`SubmissionBuild` exposes only the donor's value-free Exam form field names.
After an immutable Draft claims the v2 artifact, each
`chaoxing.exam-answer-save.v1` operation freezes the exact CxKitty-compatible
`pos`, `rd`, `value`, `_edt`, query and form body once. Core persists the
complete request digest before Native HTTP sends the POST. A successful save
must return exactly `lastUpdate|encRemain|enc`; the timestamps may not regress,
remaining time may not increase, and the cursor advances by exactly one before
the encrypted continuation rotates. There is no read-only endpoint proving an
individual temporary save, so ambiguity keeps that operation locked.

Only after every Draft Question is saved does the Provider freeze a separate
`chaoxing.exam-final-submit.v1` with `tempSave=false`, empty `qid` and
`start=0`. Its accepted JSON is a Receipt, not completion. `SubmissionVerify`
rebinds the same Course and exact Exam row. A fresh `Completed` row is task-level
recovery evidence only; it does not prove answer consistency. Per-Question
confirmation additionally requires the strictly bound preview result to repeat
every immutable Draft Question ID exactly once, in DOM/Draft position order,
with the exact `exam_mobile` type code and a visible allowlisted `我的答案`
value. Type codes 0, 1 and 3 use bounded single-choice, sorted multi-choice and
exact boolean grammars. Missing/extra fields, ID/order/type drift, unsupported
type 2 result text or absent visible answers produce `Inconclusive` with
`Unverified` Questions; duplicates fail closed. Only a fully bound value
mismatch is `Rejected`, and matching Questions can remain individually
`Confirmed`. Hidden inputs, CSS, score and list state are never answer evidence.
When the same fresh result exposes a bounded decimal score, the parser retains
its original thousandths-of-a-point integer and projects it independently as a
0-100 `SubmissionScore`; 0 and three-place fractional scores remain exact, and
missing score remains `None`. Confirmed, Rejected and Inconclusive answer
snapshots may all carry this independent score fact.
An ambiguous final submit may be recovered as accepted only when that same
fresh exact row is already Completed, after which this independent result
verification still runs. Synthetic fixtures and Provider integration tests
cover this boundary; live account validation remains pending.

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
- Chapter Work's `selectWorkQuestionYiPiYue` result is iframe-bound. Until the
  shared BrowserBridge binds course/class/knowledge/job and hands the fresh
  document to the strict Provider parser, a completed card must not be upgraded
  to per-Question verification.
- Chapter Work retakes can randomize option `data` values while retaining the
  displayed letters. Standard-answer candidates must be rebound to the fresh
  Question option IDs for that attempt; cached option mappings must never drive
  a later retake.
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

The same transport now keeps Work submission preparation and mutation separate:
it may renew and repeat only the read-only editor acquisition, then sends one
form POST with the final editor URL as Referer. Verification uses only the
existing bounded redirect reader and classifies the final editor/prompt/view
route before handing a zeroizing document to the strict result parser.
