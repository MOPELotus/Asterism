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

## Video and Audio execution checkpoint

The audited primary donor obtains fresh media metadata from the Chapter Card,
then reads the current media status before reporting progress. Its Audio request
uses the same route, signature and progress fields with `dtype=Audio` and the
versioned `/ananas/modules/audio/index_new.html` Referer. Current OCS card
evidence distinguishes Audio from Video as
`property.module=insertaudio|insertvideo` even though the outer attachment type
is `video`:

```text
GET /ananas/status/{objectId}?k={fid}&flag=normal
  -> status + dtoken + duration + optional playTime

GET /mooc-ans/multimedia/log/a/{cpi}/{dtoken}
  ?clazzId&playingTime&duration&clipTime&objectId&otherInfo
  &courseId&jobid&userid&isdrag=3&view=pc&enc&dtype={Video|Audio}&rt&_t
```

`enc` is the lowercase MD5 of the donor-observed ordered report tuple containing
class, user, job, object, millisecond progress, the protocol constant,
millisecond duration, and clip range. Asterism constructs this only inside the
short-lived native transport. `objectId`, `otherInfo`, `dtoken`, face-capture and
attendance fields remain redacted, zeroizing execution material and never enter
Task snapshots, logs or results.

The Core-owned Execution snapshot supplies a bounded playback rate from 1.0 to
2.0 and a 30-90 second media-progress interval. Media time advances
monotonically; the real wait for each interval is divided by the frozen playback
rate. Retry starts from freshly reported server/Card progress, not from a
persisted token. HTTP/JSON success is still provisional: the complete seven-card
matrix is re-fetched and the exact stable resource identity must report
`isPassed = true`.

Samueli currently falls back from Video to Audio after an undifferentiated
Video failure because its own parsed job shape does not retain the module. That
behavior is not ported: a progress request may already have remote effect, so
Asterism issues Audio only from the fresh structural `insertaudio` fact and
fails closed when the media kind cannot be established.

The donor includes automatic captcha solving. The current Native HTTP boundary
turns a captcha/validation response into typed `HumanRequired(ImageCaptcha)`
before any blind retry; the donor solver and a bounded Capture/BrowserBridge
handoff are active first-batch work. The endpoint, signature, current Referer
version and real account behavior remain live-test gates; this checkpoint is
offline/native-boundary evidence only.

## Live execution checkpoint

The refreshed primary donor separates one read-only duration lookup from a
minute-like heartbeat mutation loop:

```text
GET https://mooc1.chaoxing.com/ananas/live/liveinfo
  ?liveid&userid&clazzid&knowledgeid&courseid&jobid&ut=s
  -> temp.data.duration

GET https://zhibo.chaoxing.com/saveTimePc
  ?streamName&vdoid&userId&isStart=0&t&courseId
  -> @success
```

Asterism rediscovers the exact fresh seven-card target and keeps `liveId`,
`streamName`, `vdoid` and the optional status job value only in zeroizing,
redacted execution state. The strict nonzero duration is capped at 24 hours;
the frozen bounded playback rate determines both the heartbeat count and the
59-second donor cadence. The `_uid` used by `liveinfo` is frozen in the status
object and every later session must expose the same identity.

Each heartbeat uses Core's ordered `ExecutionMutationSink`. Before send, the
Provider records `chaoxing.live.heartbeat`, its 1-based ordinal and a
domain-separated SHA-256 digest binding GET, stable resource job, full request
URL including timestamp, and Referer. A definite bounded response records a
separate response digest plus accepted bit before another ordinal may issue.
One definite rejection may be retried after the donor's five-second delay under
a new ledger ordinal. No session renewal occurs after the first heartbeat;
network, parsing, receipt or later pre-send failures become `HumanRequired` and
are never replayed.
`@success` remains only a mutation receipt. Completion is accepted solely when
a separate fresh `TaskProgressRead` rediscovers the exact card as Completed.
All current evidence is synthetic/offline pending a sanitized live-account run.

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

`TaskProgressRead` preserves the same module split. Document, Read, Video, Audio
and Live Resource tasks retain the targeted fresh-card lookup used for crash
recovery. Chapter, Work and Exam tasks use exact Task rediscovery; `Completed`
and `Pending` expose only binary 100/0 completion while all other remote states
leave percentage absent.
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
short-answer. Fill submission requires the fresh page to expose exactly one
bounded blank and the Draft to contain exactly one text value; multi-blank
concatenation is ambiguous and fails closed. Short-answer accepts one text
value and other shapes fail closed.

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
visible answer values for single-choice, multiple-choice, true/false and a
`blank_count=1` fill Question. It
emits per-Question Confirmed/Rejected facts and retains the independent 0-100
score as fixed thousandths on any verification status. A pending submitted
fill value is not evidence. Missing, extra, reordered, multi-blank/short-answer
or unsupported evidence remains Inconclusive;
duplicates and malformed or conflicting scores fail closed. The parser is an
offline boundary only until Main's BrowserBridge can supply the fresh bound
study-page iframe document. The completed card alone continues to produce only
task-level confirmation.

The same strict result boundary parses `正确答案` independently from the user's
submitted value. When every result Question matches the current Question set by
QID, DOM order and type, single-choice, multiple-choice, true/false and exact
single-blank standard values become full-confidence `ProviderNative`
AnswerCandidates. Each choice
must still name an option ID in the fresh Question snapshot; the candidate
provenance records only the result-route schema and remote QID. Missing standard
answers, pending/multi-blank/short-answer types, reordered Questions or unknown options fail
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

CxKitty's QR path is independently actionable. One Web
Cookie jar loads `passport2.chaoxing.com/login`, extracts bounded `uuid`/`enc`,
activates `createqr?uuid=...&fid=-1`, presents the exact
`toauthlogin?uuid=...&enc=...` payload, and polls `getauthstatus` with the same
two values. Core presents the challenge only to the authorized owner and owns
the encrypted continuation, serialized polling and atomic owner-bound
credential commit.

The Provider-owned boundary now implements that chain. `begin_qr` requires a
Core account/AuthSession/correlation binding, uses the donor Web User-Agent,
parses exactly one `uuid` and `enc`, activates one QR and returns the
`toauthlogin` payload only through a secret wrapper. The Authentication adapter
then exposes only that exact HTTPS Passport action to the current authorized
AuthSession. `poll_qr` repeats the same
binding and performs exactly one `getauthstatus` POST; no Provider loop, sleep or
automatic retry exists. Missing type means awaiting scan, type `4` means scanned
and awaiting confirmation, `1` is rejected, `2` is expired, and unknown states
fail as protocol drift. Nickname/UID/message fields are ignored and the bounded
body is zeroized. Each definite result receives a domain-separated digest over
the consumed continuation digest/revision facts, normalized state, exact
bounded response digest, canonical scoped-Cookie digest and, after validation,
the fresh identity document digest.

The manual Cookie jar validates Chaoxing-only HTTPS domain/path scope, keeps
host-only Passport Cookies off Course hosts, applies replacement/deletion and
zeroizes both stored values and assembled headers. `status=true` must yield an
`_uid`/`UID` Cookie applicable to the Course host and then rotates to a durable
identity-validation continuation. Its next poll does not call `getauthstatus`;
it performs one fresh `courselistdata` read before the transport returns an
authenticated session. If that read fails transiently, Core releases the same
continuation so a retry repeats only the read-only identity validation.

The canonical `chaoxing.qr.v1` continuation contains schema, exact
ProviderAccount/AuthSession/correlation binding, `uuid`/`enc`, sorted scoped
Cookie entries, continuation revision, last consumed Core poll sequence, phase,
absolute challenge expiry, begin-evidence digest and optional authenticated
identity digest/time. Its digest covers the exact plaintext which Core
encrypts. Revision and poll sequence are deliberately separate: a released
retryable Core claim consumes its bounded sequence without rotating Provider
state, so the next claim may have a greater sequence at the same revision. The
Provider accepts only that monotonic bounded gap, writes the claimed sequence
into the next continuation, and rejects stale/repeated sequences. Every
definite poll then either rotates to AwaitingScan/
AwaitingConfirmation/IdentityValidation or closes as Rejected/Expired. Remote
success rotates first to IdentityValidation before any fallible Course read;
verified success then rotates to an Authenticated terminal continuation. Thus a
crash or transient read failure after `getauthstatus` success cannot replay that
status request. `finalize_interactive_authentication` performs no network
request and deterministically derives the same `QrCode` Cookie bundle from that
terminal value. Non-terminal values cannot be finalized; authenticated values
cannot be polled. Waiting expiry closes the challenge, while an already
persisted authenticated terminal can still finalize after the QR display
window expires. The Development factory now advertises `AuthMethod::QrCode`;
the 2024 route remains Development-level until sanitized live validation.

## Native independent Work submission

The primary donor names every answer field `answer{qid}` and every type field
`answertype{qid}`, builds `answerwqbid`, sets an empty `pyFlag` for final submit,
and POSTs `https://mooc1.chaoxing.com/mooc-ans/work/addStudentWorkNew`. The older
mobile donor confirms the same answer/type grammar but exposes a different
global parameter set. Asterism therefore does not persist or blindly copy an
executable donor payload.

`SubmissionBuild` remains credential-free and value-free. It accepts
single-choice, multiple-choice and true/false Questions whose remote option
identities have the unambiguous donor grammar. The pinned primary donor also
maps type 2 to the same `answer{qid}` / `answertype{qid}` pair and sends one
answer field per Question. Asterism therefore accepts an independent Work fill
only when the current page binds exactly one blank and the Draft carries exactly
one text value. It validates the complete Question/SelectedAnswer binding and
returns only the two expected field names per Question. Multi-blank,
short-answer and complex Work shapes remain fail-closed until their exact
editor and result/grading semantics are established.

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
Draft answer becomes Confirmed with remote Completed. Type 2 confirmation
additionally requires the same one-blank Question binding and one exact bounded
visible text value; each selected result node must contain exactly one
`我的答案` label, and a duplicate label is structural drift rather than a safe
cutoff point. A different value is Rejected and a missing value is
Inconclusive. Choice evidence may contain only uppercase option IDs and explicit
whitespace/comma/Chinese-comma/dunhao/semicolon separators. Duplicate IDs,
multi-valued single-choice text or arbitrary surrounding text are protocol
drift rather than values to filter or deduplicate. True/false evidence must be
one complete token from the controlled Chinese/English/symbol vocabulary;
substring values such as `不正确` or `错误但对` are drift. A complete valid-value
mismatch is Rejected; incomplete result facts are Inconclusive. CSS selection
state, list text, HTTP 200 and the acknowledgement JSON never substitute for
this readback.

The same unique `我的答案` label rule is applied before mobile Exam visible-value
parsing. A node such as `我的答案：B 我的答案：AD` cannot confirm the first value or
fall through as an ordinary mismatch; it is protocol drift because the remote
answer evidence is structurally ambiguous.

Result identity attributes are exact protocol fields. Chapter
`singleQuesId[data]` and Exam `input[name=questionId][value]` are validated
without trimming; a padded value such as `" exam-q-1"` is ProtocolDrift, not a
recoverable spelling of `exam-q-1`. This matches the editor/attempt binding rule
that remote identities are never repaired before comparison.

Controlled empty/pending Work values (`未作答`, `未答`, `待批阅`, `等待批阅`,
`答案待公布`, `暂无答案` and the equivalent empty cutoff) are not malformed remote
protocol and are not answer mismatches. The parser returns no selected answer
evidence, so the whole snapshot is Inconclusive and every Draft Question remains
Unverified. Other non-empty malformed choice or true/false text remains
ProtocolDrift.

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

The cover's Exam-code flag follows the pinned CxKitty parser grammar exactly:

```text
var *needcode *= *(\d+);
```

Exactly one bounded decimal declaration is required. `0` means no code gate and
every nonzero integer means the gate is present. Missing, duplicate, quoted,
fractional or lookalike declarations are ProtocolDrift. In particular, the
Provider does not search the statement for digit `1` and does not default an
omitted declaration to false.

The cover's `faceRecognitionCompare` and `captchaCheck` hidden inputs are also
required structural gate facts. Each selector must match exactly one element,
and its raw `value` must be exactly `0` or `1`; no whitespace normalization is
performed. Missing, duplicate, padded or unknown values are ProtocolDrift. A
true code, face or captcha gate stops the Native start path as
`HumanRequired(BrowserRequired)`; this parser does not solve the challenge.

`testUserRelationId` is the attempt identity later sent as `examAnswerId`.
The cover must expose exactly one raw bounded value. The start page is allowed
to omit the field and retain the already bound cover identity, matching the
existing fallback contract; if the field is present, exactly one element must
exist and its untrimmed value must equal the bound identity. Padded or duplicate
attempt IDs are ProtocolDrift before dynamic `enc` state is accepted.

Core persists `chaoxing.exam-start.v1` and the canonical request digest before
the Provider performs one `phone/start`. The Native transport accepts only the
bound redirect and returned `examAnswerId`, then reads the complete
dynamic-`enc` mobile preview. The preview's newer `enc`, remaining-time and
last-update values supersede the start page. Success atomically materializes
normalized Questions plus the minimal encrypted
`chaoxing.exam-question-attempt.v3` artifact containing route, attempt/timing
material, Question count, ordered QID/position/content-fingerprint bindings,
the complete Question-set digest, frozen selected original positions and the
next selected-save cursor. No HTML or answer content enters the artifact. Any error after issue
remains ambiguous and cannot be auto-replayed. Cover or start responses
requiring an exam code, face check or captcha return `HumanRequired` for
BrowserBridge/Capture handoff.

`SubmissionBuild` exposes only the donor's value-free Exam form field names.
After an immutable Draft claims the v3 artifact, the Provider requires Core's
complete coverage denominator to equal the artifact Question count, matches
each selected Draft Question to its original QID/position/content fingerprint,
and freezes the strictly increasing selected positions. Each
`chaoxing.exam-answer-save.v1` operation freezes the exact CxKitty-compatible
`pos`, `rd`, `value`, `_edt`, query and form body once. Core persists the
complete request digest before Native HTTP sends the POST. A successful save
must return exactly `lastUpdate|encRemain|enc`; the timestamps may not regress,
remaining time may not increase, and the selected cursor advances by exactly
one before the encrypted continuation rotates. A save's `start` is the selected
Question's original zero-based full-paper position, not its index in the Draft.
There is no read-only endpoint proving an
individual temporary save, so ambiguity keeps that operation locked.

Only after every selected Draft Question is saved does the Provider freeze a separate
`chaoxing.exam-final-submit.v1` with `tempSave=false`, empty `qid` and
`start=0`. Its accepted JSON is a Receipt, not completion. `SubmissionVerify`
rebinds the same Course and exact Exam row. A fresh `Completed` row is task-level
recovery evidence only; it does not prove answer consistency. Per-Question
confirmation additionally requires the strictly bound preview result to repeat
the v3 artifact's complete ordered QID/position binding with bounded type fields. Every selected
Draft Question must occur at its original DOM position with the exact
`exam_mobile` type code and a visible allowlisted `我的答案` value. Type codes 0,
1 and 3 use bounded single-choice, sorted multi-choice and exact boolean
grammars. Type 2 additionally requires the parsed Question to bind exactly one
blank and preserves one bounded visible text value byte-for-byte; empty or
pending-label values are not evidence. Unselected Questions may use other numeric types or omit visible
answers; they remain structural count/identity evidence only. Missing/extra
fields, selected ID/position/type drift or absent selected visible answers
produce `Inconclusive` with `Unverified` Questions; duplicates fail closed. Only a fully bound value
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

## Coverage, evidence corpus and retake checkpoint

Samueli's `submit` and `cover_rate` are semantic submission policy, not form
fields. Its default coverage is 0.9. OCS independently computes
`finished / total Questions` and offers save-only, no-move, forced and numeric
threshold modes. Asterism must calculate coverage over the complete fresh
QuestionSnapshot, preserve unsupported/unanswered Questions in the denominator,
and freeze the resolved threshold plus exact submitted/unanswered identities in
the immutable Draft. Save-only and final submit are distinct intents. A donor
rollback which submits below threshold to unlock a prerequisite is a separate
explicit execution policy, never an implicit coverage bypass.

Core now freezes that coverage partition. For independent Work and Chapter Work,
the Provider rebuilds one fresh complete form: `totalQuestionNum` and the parsed
Question count must equal the immutable denominator, every selected QID/type must
still exist, and the complete current DOM order is retained in `answerwqbid`.
Selected values come only from the Draft; every unanswered `answer*` is emitted
empty with its fresh numeric `answertype*`, so neither old editor state nor a
random donor fallback can leak into the final submit. Independent Work result
verification again requires the complete count and binds each selected QID/type
to its original snapshot position before comparing the visible value. Unselected
rendered controls are not promoted to answer evidence.

The mobile Exam path remains a separate request family. Its encrypted v3
artifact binds every original QID/position/content fingerprint and the complete
Question-set digest, while Core's partial Draft carries selected Question bodies
plus the complete denominator and unanswered identity partition. On first Draft
binding the Provider freezes selected original positions; later continuation
revisions must reproduce the same selection exactly. Saves advance only this
selected cursor and use the original zero-based paper position in `start`.
Verification still requires the complete remote result count, but unsupported
unselected types and missing unselected visible answers do not become evidence
and do not make otherwise exact selected evidence inconclusive.

Completed historical Chapter Work, independent Work and Exam rows may be
enumerated read-only. A result fetch must remain bound to
owner/account/course/task/attempt and must not call any attempt-creating route.
The parser must retain these independent facts:

- task completion and score;
- the user's visible submitted answer;
- an official standard/reference answer and its exact label;
- per-Question correctness or pending-manual-grading state;
- structural retake availability.

Only the latter two answer facts can upgrade evidence: an explicit standard is
Official/ProviderNative, while a correct marker can make the submitted value
VerifiedHistorical without changing its original Manual/ExternalBank/other
source. Normal SubmissionVerify may append the same evidence incrementally.
The owner-level corpus, evidence status and bootstrap lifecycle are Main-owned
and are not represented by the current `AnswerCandidate` contract.

The Provider exposes a typed Chapter Work history parser
over the already bounded `selectWorkQuestionYiPiYue` document. For the supported
0/1/3 plus exact single-blank type-2 subset it requires complete
QID/order/type/current-option-or-blank-count binding and both
visible labels, then returns the submitted value, official value, exact
per-Question equality judgement, fixed-point score and an optional exact
`redoTest(...)` entry. It does not construct Private Evidence, infer owner or
Attempt identity, classify an overall score as per-Question evidence, or invoke
the retake. Answer values are redacted from `Debug`.

`ChaoxingAnswerHistoryHarvest` now adapts that parser to Core's read-only
history contract. Its private transport pages only completed Chapter Work
records and supplies the audited attempt-local `workAnswerId` without exposing
an endpoint. The Provider length-prefix hashes schema, stable Task/Course
identity and `workAnswerId` into `provider_attempt_digest`; the exact bounded
result document receives a separate domain-separated `result_digest`. Read
reconstructs Questions with Core's supplied TaskId and preserves submitted,
official, equality, score and structural `RedoTest` facts. Cursor state contains
only a sanitized increasing page ordinal. The default Native factory does not
advertise this capability until BrowserBridge/Capture provides a verified
read-only list/result implementation; the synthetic transport is test-only and
does not invent a native endpoint.

## Verified completion diagnosis

The current TaskExecution paths return a successful `ExecutionOutcome` only
after a fresh card/status read proves Completed. Human challenges, unsupported
resources and protocol failures are typed errors rather than verified
incomplete outcomes, so TaskExecution has no evidence-backed diagnosis override.

SubmissionVerify can return an explicit fresh `RemoteState::Expired` from the
bound Work/Chapter Work/Exam inventory. That fact maps exactly to
`CompletionDiagnosis::WindowClosed`. No other current snapshot state is safely
specific enough: `NotOpen` can mean a time gate or an upstream prerequisite,
and Pending/InProgress can mean an unchanged editor, an active attempt or
teacher review. Those states return no Provider override and Core retains its
conservative `RemoteUnknown` diagnosis. A completed result always wins in Core,
and score or answer mismatch is not reclassified as a completion reason here.

## Remaining activity and Browser-only mutations

Samueli commit `741d5c1884aa84ea50c650897c870229c1de812e` adds the
low-level activity-list, `preSign` and `pptSign/stuSignajax` requests plus
normal/gesture/location numeric modes. It also adds an `--auto-sign` argument,
but no caller dispatches that argument or classifies an activity into a safe
sign-in command. The final request returns opaque text and the donor performs
no independent activity readback.

The current pinned `Ylim314/chaoxing-sign` and `superdaobo/mini-hbut` sources
independently corroborate these read-only calls:

```text
GET https://mobilelearn.chaoxing.com/v2/apis/active/student/activelist
    ?fid={fid}&courseId={courseId}&classId={classId}
    &showNotStartedActive=0&_=...
GET https://mobilelearn.chaoxing.com/newsign/signDetail
    ?activePrimaryId={activityId}&type=1
```

The list envelope is `result=1 -> data.activeList`. Sign rows carry `type=2`;
activity/course/class identities may be numeric or decimal strings, and times
are observed both directly and as typed `Time.time` objects. Asterism now has
an unregistered Provider-local Native reader for these two GETs. Each bounded,
zeroizing document is parsed atomically, list rows are bound to the exact fresh
course/class route, duplicates fail closed, and `signDetail` must retain the
same activity/type/`otherId` and any jointly present start/end time.

The donors do not agree on the higher-level semantics: `status=1` means
`Doing` in `chaoxing-sign` but is treated as `Signed` by `mini-hbut`, while
`otherId=5` is code-sign in the former and photo-sign in the latter. The parser
therefore keeps status/user-status as raw bounded codes and represents
`otherId=5` as `AmbiguousCodeOrPhoto`. It does not infer account completion or
mutation eligibility. `ifPhoto=1` on `otherId=0`, and `otherId=0/2/3/4`, are the
only non-conflicting normal/photo/QR/gesture/location classifications.

`signDetail` is an activity-structure read, not an independent account sign
result. Registration still requires a Core sign-in inventory/operation
contract, sanitized `preSign` and result fixtures, explicit object/location
inputs, durable non-idempotent authority, receipt classification, and a fresh
account-bound readback which proves the exact activity was completed. No
`preSign` or `stuSignajax` call exists in the current implementation.

The Provider-local ordinary-sign preparation closes only one exact donor request
family. From one exact list/detail pair and explicit bounded UID/FID/name, it
identifies `YlimPreSignStuSignAjax` and freezes two separate zeroizing query
templates:

```text
newsign/preSign:
  courseId, classId, activePrimaryId, general=1, sys=1, ls=1,
  appType=15, uid, isTeacherViewOpen=0

pptSign/stuSignajax:
  activeId, uid, clientip="", latitude=-1, longitude=-1,
  appType=15, fid, name
```

This is Ylim's exact ordinary pair, not a donor-field union. Samueli's pre-sign
shape adds `tid=""` and `ut=s` but omits `isTeacherViewOpen`; its terminal
`stuSignajax` also carries extra course/class/object/user-agent fields.
`mini-hbut` uses the broader pre-sign shape and then a different `/pptSign`
family, with its current common-sign caller passing placeholder pre-sign
identities. Those sources corroborate overlapping behavior but do not authorize
splicing their fields or routes into Ylim's pair.

The resulting digest domain is versioned and binds the explicit protocol
family, actor fields, activity/detail fingerprints and both canonical queries.
Only the family, static route names, two-request count and digest leave the
object; the zeroizing queries have no public getter and no transport
implementation consumes them. Status codes do not authorize the preparation.
Photo upload/object identity, location fields, gesture/code prerequisites and
QR `enc` differ or require extra dynamic evidence across the donors, so every
non-normal variant remains unsupported at preparation time.

Ylim's fixed implementation contains an additional donor-specific auxiliary
path inside `preSign()` which the earlier ordinary-core audit omitted:

```text
pptSign/analysis:
  vs=1, DB_STRATEGY=RANDOM, aid={activityId}

bounded raw response:
  exactly one code='+'{dynamicCode}' marker

pptSign/analysis2:
  DB_STRATEGY=RANDOM, code={dynamicCode}
```

The implementation invokes this pair unconditionally before variant dispatch,
but its own comment says the pair is required for location sign-in. Asterism
therefore models it separately from the exact Ylim ordinary pair: one fresh
ordinary preparation binds the first `analysis` query; one bounded, zeroizing
response parser rejects missing/duplicate/empty/unsafe codes and freezes the
dynamic `analysis2` query. Only routes/counts and request/response digests are
public. No request is sent, the opaque `analysis2` response has no parser, and
the unconditional donor implementation is not promoted into a claim that the
server universally requires these calls for ordinary sign-in.

The pinned Ylim source states that the `preSign` request must be sent before a
later sign record can be created, and its `handleSign` path always issues that
request before dispatching a variant. `preSign` is therefore not classified as
read-only merely because the remote method is GET. It is the first protocol
step of the non-idempotent sign sequence. Any future transport must persist its
issue before I/O, bind it to the exact fresh activity/detail/actor snapshot,
separate its observation from any selected `analysis`/dynamic-`analysis2` step
and the later `stuSignajax` Receipt, and never replay an ambiguous issued step
unless independent donor evidence establishes a safe recovery rule. The current
auxiliary preparation records the exact donor path but does not decide whether
that path belongs in an ordinary-sign transport sequence.

Bounded pure parsers cover the donor-observed responses without adding a
transport. The pre-sign HTML must contain exactly one structural
`#statuscontent`: empty text becomes `NoCompletionMarker` and exact `签到成功`
becomes `AlreadySigned`; missing, duplicate or any other non-empty text is
protocol drift. The separately preparation-bound `stuSignajax` body recognizes
only:

```text
success        -> Accepted Receipt
您已签到过了    -> AlreadySigned Receipt
success2       -> WindowClosed Receipt
```

Response documents are bounded, zeroized and represented outside the parser
only by the preparation binding, response digest and controlled enum. These
three bodies are matched byte-for-byte, as in Ylim's strict equality checks;
leading or trailing whitespace is protocol drift rather than normalization.
These classifications are not a send path. `Accepted` is not verified
completion, and even `AlreadySigned` remains an endpoint/preflight response
rather than the required independent fresh account-result readback.

QR sign-in has a separately bounded Capture-input checkpoint. Ylim's fixed
source names the dynamic submit fields and its refresh path constructs a
`SIGNIN:aid=...&source=15&Code=...&enc=...` payload; `mini-hbut` independently
parses allowlisted Chaoxing HTTPS QR URLs carrying `activeId|id` plus `enc`.
Asterism accepts either carrier only after fresh list/detail rebinding proves the
same exact `QrCode` activity. The parser requires one non-ambiguous activity ID,
one bounded dynamic `enc` and a carrier-exact field family. Across all recognized
aliases, exactly one of `activeId|id|aid` may appear. An allowlisted HTTPS URL
uses only `activeId|id` and must not contain Ylim-only `Code/source`; an exact
`SIGNIN:` payload uses only `aid` and requires both `source=15` and one bounded
`Code`. It rejects userinfo, custom ports, foreign hosts, duplicate or
cross-carrier aliases/fields and foreign activities. Raw QR material is
zeroized and redacted; only source, binding digest, material digest and
refresh-code presence are public.

A second Provider-local value freezes only Ylim's exact QR submission step:

```text
pptSign/stuSignajax:
  enc, name, activeId, uid, clientip="", useragent="",
  latitude=-1, longitude=-1, fid, appType=15
```

The preparation repeats the fresh activity/detail binding, requires the QR
material binding digest to match, validates explicit numeric UID/FID plus a
bounded actor name, and hashes the complete canonical query with the material
digest. Only the static route, one-request count and digests are visible; the
zeroizing query has no public getter and no transport consumer.

This parser does not settle the mutation sequence. Ylim requires `preSign`, its
current `preSign()` implementation additionally issues the donor-specific
`analysis`/dynamic-`analysis2` pair, and it does not wire QR dispatch from its
main handler; `mini-hbut` instead treats one different `preSign` shape as
terminal. Neither supplies an independent account-result readback. No QR
request is therefore sent until Main provides the durable multi-step authority
and a fresh verification surface. Freezing the submit request does not
authorize omitting, replaying or guessing any preceding step.

Ylim also implements event-driven discovery through a distinct WebIM path:

```text
GET https://im.chaoxing.com/webim/me
HTML: #myToken, #myTuid, #myName

Easemob connection:
  appKey = cx-dev#cxstudy
  ws     = https://im-api-vip6-v2.easecdn.com/ws
  api    = https://a1-vip6.easecdn.com

delivered text message:
  ext.attachment.att_chat_course.atype = 2
  ext.attachment.att_chat_course.aid
  ext.attachment.att_chat_course.courseInfo.courseid
  ext.attachment.att_chat_course.courseInfo.classid
```

Asterism implements only the first GET and pure parsing of an already-delivered
message. The bootstrap document is bound to Provider account and correlation;
token, IM UID and name remain zeroizing secrets with only binding/credential
digests exposed. A sign event retains the course/class/activity identity and
message digest, then still requires fresh course/activity/detail rebinding.
Other `atype` values are ignored and malformed sign events fail atomically.

Opening and maintaining the Easemob connection is not implemented. The donor
listener forwards every SDK text callback without reading a message ID or
cursor, recursively starts another listener from `onClosed`, and configures
only `autoReconnectNumMax=2` with `delivery=false`. Asterism therefore needs a
shared durable subscription owner, encrypted credential handoff, lease and
reconnect/backoff policy, ordered event de-duplication/recovery and explicit
acknowledgement state before it can consume events continuously. A message
digest is evidence of the received payload, not an ordered delivery cursor.
No WebIM event is authorization or completion.
Ylim's `sign_history` cannot close the verification gap: the endpoint reads
only its own Prisma `SignLog`, populated locally after the same opaque mutation,
and never reads a Chaoxing account result.

Samueli commit `38a269811c9bb4a44bb31beaf02c552168c50864` implements
post-task learning-count traffic by loading `studentstudyAjax`, extracting one
absolute `fystat-ans.chaoxing.com/log/setlog` script URL, sending it, sending two
monitor requests around a 30-second wait and incrementing only a process-local
counter. Missing `setlog`, a rejected `setlog` response and monitor failures do
not stop that local counter, and no donor route reads the resulting server
count. These calls cannot become verified Asterism execution until a fresh
server-visible count or equivalent exact readback is captured. Any future
implementation also needs a durable per-visit operation because replaying an
ambiguous `setlog` can over-count.

OCS `890686a5e54f9a6d52d1169bae9ea5971e0863c7` identifies a
hyperlink iframe with `#hyperlink`, binds the surrounding frame `jobid` to one
attachment, temporarily disables the element's popup handler, clicks once and
waits three seconds. It does not read completion afterward. Its timed-reader
path is likewise Browser-only: it identifies
`iframe[name="bookifame"][src*="timing"]` and drives page controls after
route-provided timing waits. Both require sanitized nested-frame fixtures, a
versioned BrowserBridge command and a separate fresh card/progress readback;
neither a click nor elapsed time is a completion receipt. OCS's in-video path
offers only random-or-ignore behavior, so it provides no acceptable answer
authority; exact Question and saved-result fixtures remain mandatory.

## Protocol drift observations

Three fail-closed Provider branches attach Core-validated structural evidence:

- SubmissionBuild records an unknown numeric Provider Question type code plus a
  controlled page-kind enum;
- TaskInventory records an unknown Chapter attachment using only known module
  classification and field-presence booleans;
- SubmissionVerify records an unrecognized Work result using only Question
  count and known selector-presence booleans.

The observations never contain source HTML/JSON, route values, task or Question
identities, status/answer text, Cookies or ephemeral credentials. Unknown raw
type/module values are deliberately collapsed to presence or `other`. Branches
which have no already-parsed safe structural fact retain their legacy sanitized
error without an observation rather than deriving one from the error message.

Strict Completion stops after independently verified completion. Score
Improvement evaluates fresh score and result-route retake eligibility under a
separate bounded policy. Chapter `redoTest` and formal Exam `reTest` create new
attempts: every use requires a fresh QuestionSnapshot and option mapping, a new
immutable Draft and new durable mutation operation. No attempt-start or submit
is replayed after ambiguity. The agent donor's pass 60, target 90 and maximum 3
are evidence-backed policy examples, not universal server thresholds.

## Extended Question protocol boundary

OCS maps 0/1/3 to single/multiple/true-false, 2 and 4-10 to rendered text
completion, 11 to matching/line, 14 to cloze children and 15 to reading
children. The existing Asterism mapping deliberately keeps code 10 Composite
because donor evidence conflicts; it must remain non-submittable until a
route-specific fixture resolves the variant. Types 11/14/15 require bounded
BrowserBridge interaction and, for 14/15/shared-option issue #297, first-class
shared context/options/children. Exact single-blank result text is now
fixture-covered for Chapter history/standard evidence and Exam submitted-value
verification. Multi-blank and short result answers remain blocked on exact DOM
and grading-label fixtures; pending manual-review labels never become answer
evidence. Unknown types stay in the snapshot as
`QuestionKind::Unknown` and lower answer coverage instead of disappearing.
