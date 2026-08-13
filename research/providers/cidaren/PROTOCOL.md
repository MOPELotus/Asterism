# Cidaren protocol notes

Audit date: 2026-08-13. These notes describe frozen donor behavior and
synthetic parser targets, not live compatibility.

The reopened `ularch@bce9559`, owner-supplied `MOPELotus@a74b4a2` and
assisted OAuth V2 handoff remain independent, additive evidence lines. See
[`DONOR_DIFFERENCES.md`](DONOR_DIFFERENCES.md) for the exact revision,
release, protocol and capability matrix; no line is used to roll back another.

## Imported token and account validation

The current H5 client sends an opaque token in the `UserToken` header. The
private donor validates it through:

```text
GET https://app.vocabgo.com/student/api/Student/Main
    ?timestamp={milliseconds}&version=2.6.1.231204&app_type=1
UserToken: {opaque imported token}
```

Donor success is outer `code == 1` and contains `data.user_info`; another code
is treated as an expired/rejected token. Account fields such as student name,
student code, school and class are display data in the donor, but Asterism does
not persist them in authentication fixtures or logs. The token has no audited
expiry or refresh endpoint. Asterism therefore keeps `ImportedToken` as one
independent path and also accepts the current donor's Capture-assisted
Composite session: one account-bound `UserToken` plus bounded
`CDR_LOGIN_INFO` crypto context acquired from the authenticated WeChat H5
origin. The current public donor's local system-proxy helper captures only the
same opaque `UserToken`; Asterism accepts that exact token-only
`AssistedSession` shape from CaptureTool/BrowserExtension as a
ProviderSpecific session without pretending it contains `jv=99` material.
The Provider exposes this as a separate version-1 declarative request-header
recipe after the richer version-2 Composite recipe. Core freezes one exact
recipe version into each bootstrap session and never combines their outputs.
`AssistedSession` and `ExternalBrowserOauth` use a visible
callback/helper challenge rather than pretending a native password flow
exists.

The donor capture helper also records `CDR_USER_SESSION` when present, but its
own completion gate requires only the token plus `CDR_LOGIN_INFO`, and the
`jv=99` decoder consumes only the latter. The declarative recipe therefore
does not turn that optional, unused observation into a required JSON field;
this avoids stalling a valid capture and avoids persisting excess browser
state.

The native transport now uses the shared non-redirecting HTTPS client. It sends
the token as a sensitive `UserToken` header, preserves the audited mobile
User-Agent plus derived `Abc`/read `Authorization-V` headers, requires JSON
Content-Type and UTF-8, and caps the account body at 64 KiB. HTTP 401/403 maps
to Authentication, 429 retains a bounded Retry-After, redirects/404 map to
ProtocolDrift and server failures remain ProviderUnavailable. Response bodies
are zeroized after validation.

The Core adapter resolves either one exact manual or token-only Capture
`ProviderAccessToken`, or an exact two-record Composite binding containing
`ProviderAccessToken` plus `ProviderCompositeSession`. Every record must match
the account, reference, session kind, acquisition method and expiry. A
token-only Capture must be ProviderSpecific and originate from CaptureTool or
BrowserExtension; AndroidHelper/NativeProviderLogin cannot be relabelled into
that shape. Missing/stale metadata maps to Authentication; storage/key failures
remain sanitized Internal errors. No unsupported automatic refresh claim is
made, but a fresh Capture session can replace an expired binding.

The original historical donor's `LoginByWechatCode` hard-codes an obsolete
code into its signature and remains lineage only. A frozen first-party
`2.7.0.260715_01` H5 asset snapshot plus a redacted live-safe exchange now
provide coherent current evidence:

```text
GET https://open.weixin.qq.com/connect/oauth2/authorize
    ?appid=wx2a694105a6abbe6d
    &redirect_uri=urlencode(https://app.vocabgo.com/student/?authorize={marker})
    &response_type=code
    &scope=snsapi_userinfo
    &state={state}
    #wechat_redirect
```

Provider generates `state` from 32 CSPRNG bytes and an independent opaque
marker from 16 CSPRNG bytes. The marker is structurally prefixed and must never
equal the official frontend sentinel `"2"`; this prevents the current Cidaren
SPA from automatically consuming the callback code. The authorization URL is
redacted/zeroized as short-lived secret-bearing output. Outside that URL the
Provider returns only independently domain-separated SHA-256 digests of state
and marker for Core's pending-login record.

The user may open the same URL in an applicable WeChat environment and return
the final URL manually. This flow has authorized Windows desktop WeChat and
Android evidence and does not require MITM or XWeb debug provisioning. The
callback parser is bounded to 16 KiB and accepts only:

```text
scheme = https
host   = app.vocabgo.com
port   = absent
path   = /student or /student/
code, state, authorize = each present exactly once
```

Both ordinary query parameters and the observed SPA `fragment?...` parameter
form are supported. Required values are bounded ASCII unreserved tokens;
userinfo, lookalike origins, explicit ports, duplicate fields, marker `"2"`
and digest mismatches fail closed. The callback URL and extracted code are
zeroized and never become a public Provider API type.

```text
POST /student/api/Auth/Wechat/V2/LoginByWechatCode

code={single-use OAuth callback code}
cpub_k={fresh P-256 public key, SPKI DER then Base64}
cpub_v=v1
timestamp={milliseconds}
version=2.7.0.260715_01
sign=md5(sorted non-empty fields + donor suffix)
app_type=1                 # interceptor-added, not signed
```

The verified assisted-bootstrap exchange sends `Authorization-v: 00` and an
exact bounded User-Agent; Asterism derives `Abc=md5(User-Agent)` locally. An
explicit bounded `Authorization-v` constructor remains for the alternate
Capture/browser path, without treating that context as a durable credential.
The login request also sends a sensitive empty `UserToken`, matching the
current interceptor's `current token or empty string` shape without ever
inheriting a stale account token.
The response supplies `spub_k` and `salt`; the client performs P-256 ECDH,
HKDF-SHA256 with `info="vcg-auth-aes"`, then AES-256-GCM with exact AAD
`vcg-auth:POST:/Wechat/V2/LoginByWechatCode`. The decrypted object supplies the
new `token`. Asterism implements this entire callback-to-Composite path in
Rust, zeroizes owned callback/code/key/plaintext material, sends the exchange
once, and requires a fresh `Student/Main` read before persistence. The
redacted live-safe test removed the old `vcg_refresh_token`; V2 still
succeeded, issued a new refresh cookie and passed the account readback.

Core still owns a bounded owner/account/AuthSession-bound pending-login record,
TTL, two hash fields and atomic `pending -> completing` claim/consume before
calling Provider. It then records success/failure without retrying an ambiguous
POST. The resulting two-field Composite replacement is validated and stored as
`NativeProviderLogin`; the stored-session resolver accepts that atomic origin
in addition to same-snapshot CaptureTool/BrowserExtension pairs. The separate
donor Capture path remains valid, but generic isolated desktop Chromium must
not be claimed to observe PC WeChat XWeb storage.

Core validates the replacement again before its atomic credential commit.
Cidaren routes that `NativeProviderLogin` validation through the same current
OAuth version/User-Agent/`Abc`/`Authorization-v: 00` context rather than the
older mobile Capture headers; Capture-origin Composite sessions retain their
own audited header path.

## Course, class-task and ordinary study inventory

The donor reads ten class tasks per page:

```text
POST https://app.vocabgo.com/student/api/Student/ClassTask/PageTask
Content-Type: application/json

{
  "search_type": "0",
  "page_count": 1,
  "page_size": 10,
  "timestamp": 0,
  "version": "2.6.1.231204",
  "sign": "...",
  "app_type": 1
}
```

The frozen signing input uses the donor's separately audited version label and
a static suffix. Native request construction is covered by a fixed-time digest
vector; response fixtures never contain executable request signatures.

The native transport preserves the donor's unusual version split: the JSON
body uses `2.6.1.231204`, while the MD5 signing input uses
`2.6.1.240122`. Page one is read and structurally parsed before its bounded
`total` determines subsequent requests. One stored token is resolved for the
whole scan; any missing page, HTTP failure or later parser disagreement rejects
the complete inventory instead of returning a partial snapshot. Each page is
bounded to 2 MiB and at most ten records.

Each successful page contains `data.records` and `data.total`. Observed row
fields include:

```text
release_id, task_id, task_name, task_type, source,
course_id, course_name, release_time, start_time, over_time,
over_status, progress, score, time_spent
```

`task_type == 1` is a class learning/self-study task and `task_type == 2` is a
class test task. Both are routine platform activities. `over_status == 1`
means not started, `2` means active/not expired and `3` means expired according
to the donor and maintainer issue response. Completion is independently
observed through `progress == 100`; an active row with lower progress remains
pending or in progress.

`start_time` is an epoch-millisecond opening observation. Donor samples pair
`over_time` with a duration-like millisecond value. Public issue 6 also records
non-zero `time_spent` values such as `2181226` in the same rows, while donor
answer mutations submit values such as `20000` on the same field. This
cross-route evidence establishes millisecond wire semantics. Asterism keeps
`release_time`, `start_time`, `over_time` and `time_spent` separate, bounds
accumulated `time_spent` to 100 years and converts it to whole Domain seconds
only after a fresh stable-identity read. It still does not infer a close
timestamp from `over_time`.

Course inventory can be derived from the complete class-task pages because
each row carries stable `course_id` plus `course_name`. Multiple rows for one
course must agree on the normalized course title or the whole scan fails.

The selected `course_id` from `Student/Main` also drives the current/historical
donor's ordinary self-study route:

```text
GET https://app.vocabgo.com/student/api/Student/StudyTask/List
    ?course_id={selected_course_id}
    &timestamp={milliseconds}&version=2.6.1.231204&app_type=1
```

Public issue 83 contains a 2026 response with `data.course_id`,
`course_name`, Course progress/raw time/access observations and a `task_list`.
Each ordinary unit exposes `task_type == 3`, `list_id`, task name, score,
progress, raw time, access flags and a `task_id` that may remain `-1` across
every uninitialized unit. The native boundary first re-reads `Student/Main`,
accepts only a safe selected Course identity, binds that exact value into the
query and rejects a response or row that changes it. Class and study Course
views are merged by `course_id`; conflicting titles fail closed. This also
allows a selected Course with no class assignments to remain visible.

Pagination is all-or-nothing. Page one defines total rows; Asterism requires
exactly pages `1..ceil(total/10)`, a consistent total, no duplicate page, no
more than ten rows per non-final page, exactly `total` records and unique
release identities. Partial inventories fail rather than deleting unseen
remote Tasks.

## Stable identity and fresh rebinding

Current issue evidence demonstrates that `task_id` may be `-1` for class test
or self-built tasks and that stale in-memory task IDs can target another task.
Therefore the stable Asterism identity is:

```text
class-task:{release_id}
study-task:{course_id}:{list_id}
```

`release_id` is required, positive and unique in a scan. `task_id` remains a
bounded signed observation because later routes may need the current value.
For ordinary study units, the selected `course_id` and unique `list_id` replace
the unusable `task_id=-1` as stable identity. TaskDetail and TaskProgressRead
re-list the matching complete class pagination or selected study list and
select exactly one row by stable identity. A malformed identity is protocol
drift; an identity absent from the fresh inventory is RemoteChanged. The fresh
detail preserves the current `task_id`, Course/list binding and normalized
status, while progress publishes the bounded percentage/state and optional
duration. `time_spent` remains preserved as a normalized raw millisecond
observation; DurationRead converts it to seconds after the same fresh identity
binding. Future question or submission operations must perform that binding
before using `task_id`, `course_id` or task type.

## Question and mutation chain

The current donor's task flow is:

```text
GET  Student/{ClassTask|StudyTask}/StartAnswer
POST Student/{ClassTask|StudyTask}/VerifyAnswer
POST Student/{ClassTask|StudyTask}/SubmitAnswerAndSave
POST Student/{ClassTask|StudyTask}/SkipAnswer
POST Student/{ClassTask|StudyTask}/SubmitChoseWord
```

Responses may be plain data, base64 with version-specific inserted confusion
bytes, or current `jv=99` AES-GCM data. The current donor captures browser
`CDR_LOGIN_INFO`, removes the audited `h`/`a` field prefixes, derives a
per-response 32-byte key with HKDF-SHA256 using
`info = "vcg-exam-aes:{aad_last_component}"`, and authenticates AES-256-GCM
with the response AAD. Asterism now implements this path with bounded framing,
account-bound Composite sessions and zeroized context/key/plaintext.

The Provider isolates exact audited legacy variants alongside `jv=99`:

```text
jv=0
jv=2_1254
jv=2_9214
jv=2_10232
jv=2_10234
jv=3_1021
jv=3_2265
jv=3_2277
jv=99 (fresh captured or native-login HKDF/AES-GCM context required)
```

It bounds encoded and decoded data to 2 MiB, accepts only JSON objects/arrays,
removes only the exact fixed inserted-byte positions or sequential spans for
the declared variant, reverses the public donor's exact five-chunk permutation
for `3_*`, zeroizes temporary decoded bytes and rejects unknown versions
instead of guessing. The two frozen donors disagree on the `3_1021` transform,
so Asterism evaluates only those two exact candidates and accepts one unique
JSON value (or equal results), failing closed on ambiguity. Public issue 43
establishes the `2_1254` family, but its
textbook word payload is not retained as a fixture. Missing `jv=99` context is
Authentication; malformed framing/tag/plaintext is InvalidResponse.

The current clean-room protocol layer additionally freezes:

- fresh class/study identity rebinding before using mutable `task_id`;
- exact `StartAnswer` query family and class `release_id` / study `course_id`;
- `VerifyAnswer` answer/topic/timestamp/version signing;
- `SubmitAnswerAndSave` and `SkipAnswer` duration/topic signing;
- compact `SubmitChoseWord` word-map signing;
- donor-observed topic modes for single-choice, matching and text Questions;
- nested answer-tag flattening without persisting executable topic codes,
  while retaining sanitized top-level order, parent tag and child wire content
  needed to reproduce the donor's sentence-selection semantics exactly.

`SubmitChoseWord` is acknowledgement-only in the donor: it validates the
success envelope but does not decode a next Question. Asterism therefore uses
a separate bounded word-selection receipt parser. The ordinary assessment
parser continues to require decoded `data`, preventing a generic success
message from being mistaken for a valid Start/Verify/advance result.

Before `StartAnswer`, eligible learning Tasks build the donor's exact flat or
self-built grouped word map from a fresh Task-bound inventory. After start,
the Provider-private state machine emits only one remote operation at a time:

```text
SubmitChoseWord receipt (when required)
-> StartAnswer payload
-> current Question or reading card
-> VerifyAnswer payload with rotated topic_code
-> next VerifyAnswer (one matching relation at a time)
-> SubmitAnswerAndSave / SkipAnswer
-> next step or terminal receipt
```

Every command is account/task/correlation-bound and consumed on execution. A
transport error leaves the flow `Issued`; the caller must mark it ambiguous,
after which no replay is possible. Unexpected response semantics enter a
separate fail-closed terminal state. This Provider-private machine is not yet a
public Capability because Core still needs durable storage/recovery for each
issued edge. Only the donor's terminal Completed acknowledgement can be
projected into a bounded `SubmissionReceipt`; intermediate success and
word-selection acknowledgements cannot, and even the terminal receipt remains
verification context rather than proof of success.

Native answer evidence is also implemented. Inventory routes remain bound to
the fresh class/study Task; `Course/StudyWordInfo` retains only meanings,
phrases and examples; `Course/SearchWord` accepts the donor's additional
bounded literal Unicode-escape layer before extracting a prototype. Phrase
and example evidence remain separate because topic mode 32 consumes phrases,
while modes 41-44 and the 51-54 fallback consume examples. Meaning matching
uses donor direction `meaning in option`, and completion preserves prefix,
length, `+s`, then example fallback order.

The donor also performs explicit low-quality fallbacks when evidence is
present but no semantic match is found: random option for word-to-meaning,
third option for meaning-to-word and sentence choice, and the last inventory
word for completion. Asterism retains those capabilities and labels them with
low confidence/provenance. The random branch hashes the immutable Question ID
instead of drawing again on every call, so one reviewed Draft cannot silently
change before execution. For nested sentence options, "third option" means the
third top-level parent exactly as in the donor, not the third entry after
flattening; successful matches similarly resolve the example word back to the
exact child tag before building the one-Question Draft. A well-formed empty
`means` array is not reclassified as protocol drift: the parser falls through
to a non-empty `options` family when present, otherwise it retains empty
evidence so the donor's explicit failure strategy can run.

Donor timing configuration is represented by immutable runtime settings. The
default reported answer range is 2500-7500 ms, skip reports 20000 ms, and the
default inter-step delay is zero. Range choices are stable for one bound step
so crash recovery cannot silently choose a different mutation payload; Core
will schedule real delays instead of sleeping inside Provider mutation code.

`StartAnswer` is a non-idempotent remote attempt mutation, not a harmless
Question read. The Provider request and parser layers are implemented, while
registry integration waits on Core's durable AttemptStart/QuestionSession
contract so ambiguous starts are recovered/verified rather than replayed.

Question discovery, answer resolution, submission construction, mutation and
fresh verification remain separate capabilities. `topic_code` is ephemeral,
redacted and zeroized; it never enters a persisted Question or immutable Draft.
The donor's strings such as “task complete” are bounded receipt facts and do
not replace an identity-bound post-mutation read. The Provider-private
SubmissionVerify implementation recomputes the immutable preview, freshly
rebinds the exact release/list identity and confirms only 100%/Completed. The
current public donor additionally reads the post-run task score from
`ClassTask/Info`; Asterism's complete fresh class-task scan already exposes the
same release-bound score fact, which verification maps from bounded 0-100
decimal points into Core thousandth-points without inferring completion from
it. The platform exposes no audited answer-history readback, so the Task goal may be
confirmed while each Question remains explicitly Unverified; incomplete
readback stays Pending even after a terminal acknowledgement.
