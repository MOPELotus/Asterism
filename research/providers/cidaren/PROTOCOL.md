# Cidaren protocol notes

The mandatory one-time full donor re-audit is recorded in
[`FULL_UPSTREAM_SWEEP.md`](FULL_UPSTREAM_SWEEP.md). It re-enumerates all
README/config keys, implementation call sites, default branches, tags/releases,
105 public issues, inline examples and known fixtures. No additional platform
route or semantic setting was found beyond the protocol below. In particular,
historical batch/task-family controls are scheduling composition, Brotli is a
transport compatibility toggle, and UI/update/log/music state is not Provider
wire behavior.

Audit date: 2026-08-14. These notes describe frozen donor behavior and
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

The reopened public master's built-in `TokenCaptureService` is a GUI wrapper
around that token-only recipe, not a second Capture protocol. It clears
`fetch_token/token.txt`, saves the current Windows system-proxy state, enables
the fixed proxy `127.0.0.1:8888`, launches
`mitmdump -s addon.py --listen-port 8888 --quiet --set
console_eventlog_verbosity=error`, and polls the token file every 500 ms. Its
only successful result is the first observed opaque `UserToken`. Stop, success
and error paths terminate the child, wait at most three seconds before killing
it, and restore the prior system proxy. FiddlerCore files are present beside
the donor, but this service does not import or invoke them; their presence is
not evidence for an alternate executable dispatch path.

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

## BrowserBridge Capture command/result boundary

The Provider-side helper protocol is deliberately narrower than a browser
automation API. A Core-owned BrowserBridge session may issue exactly one typed
`capture_snapshot` command for the selected Capture recipe:

```json
{
  "version": 2,
  "session_nonce": "opaque-session-binding",
  "origin": "https://app.vocabgo.com",
  "frame_id": "frame-1",
  "remote_task_id": "class-task:2002",
  "sequence": 1,
  "command": {"kind": "capture_snapshot", "mode": "composite"}
}
```

The envelope version is fixed by the mode (`token_only` v1 or `composite` v2).
The visible origin, frame and stable class/study Task identity must match the
Core session, and sequence zero or replayed/foreign values fail closed. The
result uses the same binding and contains the declared fields only:

```json
{
  "version": 2,
  "session_nonce": "opaque-session-binding",
  "origin": "https://app.vocabgo.com",
  "frame_id": "frame-1",
  "remote_task_id": "class-task:2002",
  "reply_to_sequence": 1,
  "event": {
    "kind": "capture_snapshot",
    "user_token": "synthetic-token",
    "user_token_source": "request_header",
    "login_info": "{\"login_info\":{\"a\":\"...\",\"b\":\"...\"}}",
    "login_info_source": "local_storage",
    "user_session": null
  }
}
```

`token_only` rejects `login_info`, its source and `user_session`. `composite`
requires `login_info` to be a bounded JSON object accepted by
`CidarenCryptoContext::parse` (including the exact current `jv=99` prefixes),
and accepts `user_session` only as an optional bounded JSON observation. Every
secret-bearing field is redacted in Debug and zeroized when the Provider-owned
transport value is dropped.

This is a transport-only Provider type. It intentionally does not become a
`CredentialBundle`, Domain credential, durable receipt, selector/script,
arbitrary browser command or persisted event. Core must validate the already
paired Capture recipe and session, durably correlate the one-shot sequence,
then commit the resulting account-bound token/crypto material through the
Capture credential service. Cidaren's StartAnswer, VerifyAnswer,
SubmitAnswerAndSave, SkipAnswer and SubmitChoseWord remain native HTTP
mutations; no donor evidence justifies inventing a BrowserBridge replacement
for those routes.

Before a helper-side handler may inspect any browser state,
`project_browser_helper_command` consumes the bounded `SecretValue`,
authenticates it against Core's command digest, strictly decodes the
deny-unknown-fields JSON and immediately drops the opaque owner.
It independently binds the BrowserBridge session ID, actual page origin,
actual frame and `u64` dispatch sequence; validates the mode/revision pair and
the exact class/study Task identity grammar; and returns no echoed command
fields. Its closed output is only `CaptureSnapshotTokenOnly` or
`CaptureSnapshotComposite`. TokenOnly permits only the lower-case `usertoken`
request header. Composite permits that header or local/session
`CDR_USER_TOKEN`, and requires local/session `CDR_LOGIN_INFO` alternatives.
There is no selector, script or generic DOM action in the input or projection.

Each closed Capture source maps directly to one shared
`BrowserBridgeReadSource`; the runtime consumes Provider-owned
origin/header/storage-key authority instead of duplicating those literals.
After reading, the projection is consumed to build the exact
`cidaren.capture.snapshot.result` artifact. It retains the authenticated
session/origin/frame/Task/sequence bindings in zeroizing, Debug-redacted owners
and accepts only the projected ordered source shape: TokenOnly is exactly the
request-header `UserToken`, while Composite is exactly one allowed `UserToken`
followed by one allowed `CDR_LOGIN_INFO`. Missing, reordered, duplicate, extra
or foreign sources fail closed. Existing event validation checks the
reconstructed result before bounded zeroizing serialization and digesting;
the constructor cannot emit `CDR_USER_SESSION`, selectors, scripts or arbitrary
JSON fields.

The public helper lifecycle above is evidence for a shared local-helper
dispatcher, not Provider-owned process or proxy management. Such a dispatcher
must preserve and restore the exact previous proxy state, bind the one helper
process to one acquisition, consume only the selected recipe's result and
clean up on cancellation or failure. The Provider continues to own the typed,
bounded command/result validation and the exact token-only credential shape.

Before issuing a command, `CidarenBrowserBridge::capture_snapshot_command`
freshly calls the existing TaskDetail capability and requires the returned
Task to advertise `BrowserBridge`, the exact visible Cidaren origin and a
non-headless session. Browser policy revision 3 freezes the audited
`https://app.vocabgo.com/student/` start URL, its sole allowed origin and the
exact union of five read authorities: lower-case request header `usertoken`,
local/session `CDR_USER_TOKEN` and local/session `CDR_LOGIN_INFO`. This policy
revision is independent of the TokenOnly v1/Composite v2 command recipe
versions. Command issue repeats the start URL/origin/headless check, then
delegates bounded nonce/frame/Task/sequence validation to the typed command
constructor. This keeps fresh rediscovery and identity rebinding in the
Provider while leaving browser startup, access-token authentication, durable
sequence consumption and credential commit to Core.
The matching `parse_capture_snapshot_result` method repeats that fresh Task
check before accepting the result, then delegates exact nonce/frame/Task/
sequence and recipe validation to the typed parser. A stale Task cannot cause
a previously issued Capture observation to be committed under a new remote
identity.

For Core's durable BrowserBridge exchange table, the Provider exposes stable
message types `cidaren.capture.snapshot` and
`cidaren.capture.snapshot.result`. `CidarenBrowserCommandEnvelope::exchange_digest`
hashes the validated canonical command JSON; `browser_event_exchange_digest`
hashes the bounded raw result document after transport-size validation. Core
persists only these type strings and SHA-256 digests with the session sequence.
The command/result JSON, token, `login_info`, optional `user_session` and
validated `CidarenCaptureSnapshot` remain outside Domain storage and are
zeroized/committed through the existing Capture credential path. The digest is
an audit/identity check, not enough recovery authority by itself: after a
process restart, Cidaren must still recover the exact issued recipe mode,
frame, Task, nonce and sequence without trusting the helper's echoed result.
The shared action boundary therefore needs an encrypted bounded command
artifact, or an equivalent independently frozen set of reconstructible fields,
persisted before dispatch. The Provider now supplies that exact input as
`EncodedCidarenBrowserCommandArtifact`: its zeroizing `SecretValue` contains
the validated canonical command JSON and its digest is reused by the ordinary
exchange row. Decode rechecks the digest plus BrowserSession ID, Task,
sequence and selected Capture recipe before returning the typed command. The
issued wrapper can be consumed only into command + artifact + exchange, while
the completed wrapper yields snapshot + terminal exchange, so future Core
wiring need not discard durable metadata to obtain helper or credential data.
The wrapper deliberately has no consuming snapshot-only escape hatch. It can
also convert directly into credential replacement + terminal exchange, which
is the Provider-owned input pair for Main's atomic result/credential commit.
Command-only recovery is an internal step of the public post-crash
`complete_persisted_capture_snapshot_exchange` entry. It accepts only a valid
Issued exchange and resolves the encrypted command against that exchange and
Core's selected recipe, but cannot independently accept a raw result or caller
completion time. The persisted entry must first bind Core's encrypted result
artifact and receipt metadata, then delegates to the same fresh Task binding
and owned result-document completion path as an in-process exchange.
The crate root exposes no raw result document, event envelope, parser,
snapshot or snapshot-only consuming handoff. Core can issue the opaque command,
call persisted completion and consume only credential replacement + terminal
exchange; it cannot bypass the encrypted result inbox through a lower-level
Cidaren API.
Committed TokenOnly/Composite command fixtures are byte-identical to the
compact JSON stored in the encrypted artifact and hashed into the exchange;
helper dispatch and recovery therefore cannot evolve separate wire shapes.
The recovery artifact has its own 4 KiB input bound; the 256 KiB result bound
is not reused for the much smaller command schema. Issuance applies the same
bound after serialization, so Core cannot persist a command the recovery path
would later reject.
The high-level Provider adapter accepts the raw result only as an owned,
bounded `CidarenBrowserResultDocument`. Its contents are unavailable through
Debug or the public API, and the consumed transport buffer is zeroized after
parse/digest/completion even when fresh rebinding or exchange completion fails.
For durable recovery, Core first resolves the encrypted raw-result artifact
together with its persisted `BrowserBridgeResultArtifactMetadata`. Cidaren
copies those bytes only into the same bounded zeroizing owner, recomputes the
exact digest, and rejects result-type, session, sequence, digest or receive-time
drift before parsing. Raw-byte digest validation precedes UTF-8 and JSON
interpretation, so a tampered artifact cannot be misclassified as an ordinary
malformed helper response. The terminal exchange completion time comes only from
that persisted receipt metadata rather than an independently supplied caller
timestamp.

The shared `BrowserBridgeCapability` terminal hook is the runtime entry to this
path. Cidaren first calls the shared request validator, then strictly decodes
the digest-bound command artifact without accepting a caller-selected mode.
Only that command's TokenOnly/Composite variant supplies recipe authority. The
decoded session and Task remain bound to the issued exchange/request, while
its origin and frame must exactly match the validated runtime observation.
Cidaren then reuses persisted completion, including fresh context/Task policy
rebinding, consumes the resulting credential replacement + completed exchange
pair and returns it only through `BrowserBridgeCredentialResult::try_new`.
Neither result JSON nor extra caller metadata can select or upgrade a recipe.

`CidarenBrowserBridge::capture_snapshot_exchange` is the immutable Provider
adapter to that ledger. It derives `session_nonce` from the exact
`BrowserBridgeSessionId`, converts only a positive Provider-bounded sequence,
and constructs the issued record from the validated command rather than
accepting caller-supplied type or digest metadata. The matching
`complete_capture_snapshot_exchange` repeats the fresh Task and policy check,
parses the typed result, hashes that exact bounded document and returns a
terminal copy of the same exchange. The persisted recovery variant additionally
rebinds Core's encrypted command and result artifacts before invoking that same
parser. The zeroizing snapshot stays separate for Capture credential commit.
This closes command/result-record mismatch inside the Provider. The shared
trait now also declares the exact credential result type, classifies it as
terminal and validates the recovered request into an atomic credential +
completed-exchange pair. The Chromium runner dispatches the opaque command,
performs only its fixed reads and emits that typed result. What remains is not
another command/result contract: the runner's temporary profile must obtain a
real authenticated Cidaren document, or shared environment support must bridge
the donors' system-proxy/XWeb context, before live validation can complete.

After the terminal exchange is accepted,
`CidarenCaptureSnapshot::into_credential_replacement` consumes the zeroizing
snapshot. A token-only snapshot yields exactly one
`ProviderAccessToken` field with `ProviderSpecific` session kind. A Composite
snapshot yields exactly `ProviderAccessToken` plus
`ProviderCompositeSession` with `Composite` kind. The optional donor-observed
`CDR_USER_SESSION` never becomes a third credential because neither donor
uses it for authentication or response decoding. Core still supplies and
validates the Capture acquisition/session/account metadata before the atomic
SecretStore commit.

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
own audited header path. The acquisition context remains a non-secret property
of the resolved Provider session, so later stored-session validation also uses
the same current OAuth header family instead of silently changing semantics
after persistence.

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
pending or in progress. Any other class `task_type` or `over_status` fails the
complete inventory and emits only its parsed integer as a bounded
`TaskInventory` protocol observation; the surrounding row is never retained.

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
allows a selected Course with no class assignments to remain visible. An
ordinary-unit `task_type` other than the evidenced integer `3` likewise fails
closed with only that integer attached as `TaskInventory / UnknownTaskType`.

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

Five high-value fail-closed branches attach Core protocol observations without
retaining response or user data:

- an unknown numeric `topic_mode` records only `{ "topic_mode": i64 }` on
  `QuestionParse / UnknownQuestionKind`;
- an unknown response `jv` records only whether the value is ASCII and its byte
  length on `Other / EndpointVersionDrift`; the `jv` string itself is omitted;
- an unknown numeric class-page `task_type` records only
  `{ "task_type": i64 }` on `TaskInventory / UnknownTaskType`.
- an unknown numeric ordinary-study `task_type` records the same bounded shape
  and classification without retaining its `StudyTask/List` row;
- an unknown numeric class-page `over_status` records only
  `{ "over_status": i64 }` on `TaskInventory / FieldDrift`.
- malformed or unrecognized assessment-step/word-selection envelopes record
  only a fixed family, root/code/message/data/jv value kinds, an optional
  numeric code and data truthiness on `Other / UnknownResultShape`.
- malformed post-run task-score envelopes and decoded score aliases record only
  root/code/data/jv kinds plus an optional numeric code, or the three alias
  kinds/count, on `SubmissionVerify / UnknownResultShape`; score values and
  unrelated decoded fields are omitted.
- malformed class-page and selected-Course study-list envelopes record only
  root/code/data/records/list field kinds, an optional numeric code and bounded
  structural counts on `TaskInventory / UnknownResultShape`; no row, identity,
  title, score or progress value is retained.

These observations decorate the existing `ProtocolDrift` or `InvalidResponse`
failure only. They do not authorize guessing, retrying a mutation or accepting
the new shape. No raw
response, localized message, response-version string, Question text, answer,
topic code, remote identity, credential, Cookie or token crosses this boundary.

The current clean-room protocol layer additionally freezes:

- fresh class/study identity rebinding before using mutable `task_id`;
- exact `StartAnswer` query family and class `release_id` / study `course_id`;
- `VerifyAnswer` answer/topic/timestamp/version signing;
- `SubmitAnswerAndSave` and `SkipAnswer` duration/topic signing;
- compact `SubmitChoseWord` word-map signing;
- donor-observed topic modes for single-choice, matching and text Questions,
  plus issue 99's mode-73 multi-blank structural parse;
- optional integer `topic_done_num/topic_total` progress counters observed by
  the reopened donor for its live progress display;
- nested answer-tag flattening without persisting executable topic codes,
  while retaining sanitized top-level order, parent tag and child wire content
  needed to reproduce the donor's sentence-selection semantics exactly.

Public issue 99 contains a current `topic_mode=73` payload which the donor
explicitly does not implement. Its redacted structure has `answer_num=2`, two
positive `w_lens`, no options and one ordinary rotating `topic_code`.
Asterism parses this as `QuestionKind::FillBlank`, requires the answer count to
equal the bounded word-length count and keeps the token ephemeral. The common
`SkipAnswer` mutation remains available, so the attempt can continue. No
evidence defines whether two answers are comma-joined, separately verified or
encoded another way; AnswerResolve and ordinary answer Build/Verify therefore
fail closed for mode 73 instead of inventing a score-affecting wire format.
The distinct explicit Skip Draft contains no guessed answer fields and remains
executable through the independently evidenced `SkipAnswer` route.

The remote topic counters are not the state machine's local `position` and do
not participate in Question identity, answer selection or mutation routing.
When `topic_total > 0`, Asterism accepts an absent completed count as zero and
requires `0 <= topic_done_num <= topic_total <= 100000`; absent/zero pairs mean
the donor did not publish progress. Positive-without-total, negative,
fractional, textual, oversized or inverted counters are protocol drift. The
same observation is retained for ordinary Questions and mode-0 reading cards.
Core's durable QuestionSession contract must persist/expose it separately from
the one-shot attempt position when the public capability is integrated.

`SubmitChoseWord` is acknowledgement-only in the donor: it validates the
success envelope but does not decode a next Question. Asterism therefore uses
a separate bounded word-selection receipt parser. The ordinary assessment
parser continues to require decoded `data`, preventing a generic success
message from being mistaken for a valid Start/Verify/advance result. The
shared donor handler accepts `code=1`, `code=20001` when `data` has
Python-truthy content, or terminal `code=20004`. Public issue 72 independently
records one platform remote-state exception: `code=20001`, exact
`msg=需要选词！`, `data=null` from `StartAnswer`. The Provider accepts only that
exact code/message combination as `WordSelectionRequired`; other empty/null/
false/zero `20001` data fails closed. The same typed receipt after
`SubmitChoseWord` requires fresh word-plan rediscovery rather than issuing
`StartAnswer`. A terminal selection receipt closes the attempt and proceeds
only to fresh verification. Other localized completion/word-selection text is
classified only after numeric success; `code=0` cannot become a receipt by
copying donor UI text.

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

Every command is Provider/account/Task-bound and consumed on execution.
Correlation IDs are validated audit metadata, not recoverable identity. A
transport error leaves the flow `Issued`; the caller must mark it ambiguous,
after which no replay is possible. Unexpected response semantics enter a
separate fail-closed terminal state. The pre-Question selection/start/reading
subset is now exposed by the public `QuestionInventoryCapability` adapter and
Core has durable storage/recovery for those issued edges. The post-
materialization Verify/advance/skip subset is now exposed by a registered
session-aware `SubmissionExecute` adapter. It freshly rebinds the Task,
recomputes the immutable Draft preview and restores either the encrypted
question artifact or an intervening pre-question artifact before freezing one
exact command. Only
the donor's terminal Completed acknowledgement can be
projected into a bounded `SubmissionReceipt`; intermediate success and
word-selection acknowledgements cannot, and even the terminal receipt remains
verification context rather than proof of success. The receipt timestamp is
captured when the one-shot transport returns and travels with its bound
outcome; delayed acceptance or recovery cannot replace it with a later
wall-clock time.

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
default inter-step delay is exactly 2 seconds, matching both the reopened
donor config and its UI-enforced lower bound. Separately, the donor waits 1-2
seconds after the final `VerifyAnswer` before `SubmitAnswerAndSave`, and a
reading card resides for 1-3 seconds before the same advance mutation. Issued
commands expose those distinct, stable pre-execution delays so Core can durably
record the one-shot command before scheduling it. Range choices are stable for
one bound step so crash recovery cannot silently choose a different mutation
payload or residence time; Provider mutation code never sleeps internally.
Core also supplies the frozen issue time before the flow enters `Issued`.
The Provider constructs the complete path/query or path/body/signature at that
point and exposes a versioned operation type plus SHA-256 request digest for the
durable ledger. Native transport accepts only this prepared request and no
longer calls the wall clock while assembling assessment mutations, so the
persisted intent identifies the exact credential-free request that can reach
the platform.
When a bounded response arrives, Native transport hashes the raw bytes before
parser sanitization and zeroization, records the actual receive time and emits
both only with the strictly parsed response. Core can therefore finish the
matching ledger operation with the exact non-zero result digest. An accepted
`VerifyAnswer` rotates only the artifact's topic code; local/remote Task,
Question, position and content fingerprint bindings stay immutable, and phase
labels remain Provider-scoped as `cidaren.*`.

Skip is a persistent `NormalizedAnswer::Skip` command intent, never an empty,
Unknown or fabricated answer value. Its Draft preview has a distinct format
containing only `skip.topic_code` and `skip.time_spent`; the execution adapter
branches to `SkipAnswer` before the answer encoder. If an answer has already
entered ReadyToVerify/ReadyToAdvance, replacing it with Skip cannot reconstruct
the immutable relation queue and fails closed.

Cidaren exposes no donor setting or wire behavior for partial submission. Its
sequential protocol always binds one current Question to one immutable Draft;
an explicit Skip is still a complete, reviewable intent for that Question, not
an unanswered snapshot member. After generic Domain validation, both
SubmissionExecute and SubmissionVerify therefore require coverage 1000/1000,
an empty unanswered-ID set and `total_question_count == items.len()`. A
structurally valid generic partial Draft is rejected before preview rebuilding,
fresh remote reads or mutations. This Provider constraint is independent of
Chaoxing's evidenced partial-submit policy and adds no Cidaren runtime setting.

After an accepted post-materialization operation, the Provider classifies the
state once. A rotated token for the same Question yields Continue with a new
artifact revision. A newly parsed Question yields `NextQuestion` material for
an atomic old-session/new-snapshot transition. Reading cards or a repeated word
selection requirement rotate to `cidaren.pre-question-attempt.v2` and continue
one operation at a time until a real next Question appears. Definite Completed
yields only its receipt/digest/time and no replacement artifact. Ambiguous
transport has no donor readback and returns no recovery outcome.

`StartAnswer` is a non-idempotent remote attempt mutation, not a harmless
Question read. The Provider request/parser layers and public
`QuestionInventoryCapability` pre-Question adapter are implemented. The
adapter returns an encrypted initial continuation, freshly restores and
rebinds it before freezing one exact command, then maps an accepted result to
another continuation, a real-Question materialization, or a typed terminal
completion. An ambiguous start returns no recovery outcome because no donor
current-attempt readback is evidenced; it is never replayed.

Question discovery, answer resolution, submission construction, mutation and
fresh verification remain separate capabilities. `topic_code` is redacted and
zeroized; it never enters a persisted Question or immutable Draft. For durable
recovery after a Question exists, the Provider encodes only the local/remote
Task binding, remote Question ID, position, complete content fingerprint and
one-time code and bounded accepted-Verify count in the versioned
`cidaren.question-attempt.v2` artifact. Core
checks its stable digest and stores the plaintext only through the encrypted
QuestionSession continuation boundary; Provider decoding repeats every binding
check before reuse. For matching Questions, recovery deterministically rebuilds
the exact relation queue from the immutable Draft selection and removes only
the prefix identified by `verified_steps`; a zero, excessive or phase-
inconsistent count fails closed. No answer value or relation text is added to
the artifact. This post-start artifact does not make `StartAnswer`
replayable: the non-idempotent mutation before the first Question still needs
the separate Main-owned pre-question AttemptSession and ambiguity recovery.
The Provider now emits a bounded `cidaren.pre-question-attempt.v2` encrypted
artifact for that shared boundary. Its phases are
`cidaren.ready-to-select-words`, `cidaren.ready-to-start` and
`cidaren.reading-card`. The word-selection phase deliberately omits the
potentially large word map and requires fresh Task-bound rediscovery on
recovery. Both selection and ready-to-start retain the bounded position already
chosen by the preceding step, preventing a mid-attempt reselection from
returning to position 1. Legacy v1 ready artifacts lack enough information to
distinguish initial from already advanced state and therefore fail closed
instead of guessing or replaying a mutation. Reading-card retains only
its zeroizing topic code, stable card identity, sanitized stem, position and
optional progress. Decode repeats local/remote Task binding. A new audit
correlation can therefore resume the same account/Task state without becoming
part of its identity hash.
The registered adapter also handles a definite `WordSelectionRequired`
response by rotating back to `cidaren.ready-to-select-words`. It does not
perform another read inside acceptance or persist the old map; the next
prepare call freshly rebinds Task detail and inventory before producing a new
one-shot `SubmitChoseWord`. A definite `Completed` response is represented by
the Core typed terminal outcome with its exact raw response digest and bounded
receipt, never by an empty Question or rejection.

Both current donors retry `StartAnswer` after receiving an unknown `jv`
response, and rerunning their GUI enters `StartAnswer` again. This establishes
donor retry behavior after a definite response, not safe replay after an
ambiguous network outcome. Asterism continues to mark an unknown transport
outcome ambiguous and does not issue the mutation again.
The donor's strings such as “task complete” are bounded receipt facts and do
not replace an identity-bound post-mutation read. The Provider-private
SubmissionVerify implementation recomputes the immutable preview, freshly
rebinds the exact release/list identity and confirms only 100%/Completed. The
current public donor additionally reads the post-run task score from
`ClassTask/Info` or `StudyTask/Info` using version `2.6.1.240122`. Asterism now
issues that exact authenticated GET only after fresh Task rediscovery and binds
class requests to `task_id + release_id`, or ordinary-study requests to
`task_id + course_id`. The donor creates this session without copying its
`UserToken`; Asterism corrects that implementation defect by using the same
account-bound native session as the fresh read. The response aliases
`score/task_score/grade` are parsed as one bounded decimal 0-100 fact,
conflicting aliases fail closed, and the result maps to Core thousandth-points
without inferring completion from it. A study row with `task_id=-1` cannot be
uniquely identified by the donor request because it omits `list_id`; Asterism
retains the already fresh `StudyTask/List` score instead of making an ambiguous
cross-unit request. A missing Info score similarly preserves that fresh list
observation.

The platform exposes no audited answer-history readback, so the Task goal may
be confirmed while each Question remains explicitly Unverified; incomplete
readback stays Pending even after a terminal acknowledgement.

This also fixes Cidaren's boundary for the owner-global Answer Evidence Corpus.
Neither first account binding nor normal post-submission verification can
harvest a platform-attested per-Question answer: inventory/Info reads expose
only current Task state and an optional task-level score, while `VerifyAnswer`
exposes only the next rotated `topic_code`. The current Question's
`chance_num`/`answer_state` fields are not attempt history or correctness
receipts. AnswerResolve's word/phrase/example candidates remain useful
source-derived evidence, but cannot be relabelled as verified historical
outcomes.

The completion and improvement policies are independent. Strict Completion is
fully observable through a fresh, consistent `RemoteState::Completed` plus
`progress == 100`; neither expiry nor score proves it. The donors specify no
passing-score threshold. Their historical `95+ correctness` claim is expected
solver quality, and the current ordinary-study `progress <= 97` selector is a
filtering quirk rather than a 97% terminal rule. No audited route or field
enumerates attempts, resets a completed Task or states retake eligibility;
rerunning the GUI merely calls `StartAnswer`, and completed class Tasks are
filtered from execution. Therefore Score Improvement cannot safely start a
Cidaren retake until new upstream/live evidence establishes its allowed range.

For an incomplete fresh SubmissionVerify snapshot, Cidaren exposes one reliable
shared diagnosis. Donor class `over_status == 3` explicitly means expired, so
`Inconclusive + Expired + 0..99%` maps to `WindowClosed`. Completed snapshots
need no diagnosis. `NotOpen/Pending/InProgress` plus task-level progress below
100 proves only that the Task is unfinished: it does not carry the current
attempt's `topic_done_num/topic_total` or identify one required child. Those
states, `RemoteState::Unknown` and any tuple not emitted by the verifier return
no Provider override and remain Core `RemoteUnknown`.

This mapping does not use task score: no passing threshold is evidenced. The
donor exposes accumulated `time_spent`, but no required-duration threshold or
duration-failure result fact, so progress cannot be diagnosed as
`DurationInsufficient`; the same absence prevents `RequiredChildrenPending`.
No audited field establishes a prerequisite lock, teacher review, human action,
attempt limit or retake rule. Cidaren also has no registered TaskExecution slot;
only the verified submission readback supplies completion-diagnosis facts.

The later Core Strict Completion attempt ledger does not add a Cidaren retry
protocol. A verified `WindowClosed` diagnosis and Core's fallback
`RemoteUnknown` are stopping diagnoses, so neither can schedule another
Cidaren assessment attempt. The Provider deliberately does not relabel
unfinished progress as `RequiredChildrenPending` or `DurationInsufficient`
merely to make the shared workflow continue. Another `StartAnswer` after an
incomplete or ambiguous result still requires ordinary user/product authority
and an evidenced current session; it is not a retake primitive.

`task_type == 2` remains the donor's routine class-test family and maps to the
separate `SourceType::Exam` module. No audited row/result field establishes the
independent assessment-nature fact needed to change `AssessmentClass` to
Formal. Source classification therefore is not used as a substitute for
formal-assessment or retake policy.

`AnswerResolve` and `SubmissionVerify` are independent public Provider
capabilities. Answer resolution freshly binds the Task's Course/unit inventory,
loads only the evidence needed by each normalized Question and returns bounded
`AnswerCandidate` values without issuing assessment mutations. Through the
session-aware Core hook it first requires one current Question and the exact
`cidaren.question-attempt.v2` type, initial phase and revision, then verifies
the decrypted digest plus local/remote Task, Question ID, position and full
content fingerprint before any evidence route is called. A missing, rotated or
foreign continuation fails closed rather than resolving an answer detached
from its later mutation state. Submission
verification recomputes the immutable preview, rebinds the Task and reads
completion/progress/score without replaying any mutation. The public
mutation-backed `QuestionInventory` adapter is registered and fixture-covered;
it materializes normalized Questions directly rather than inventing a
read-only `QuestionParse` route the donor does not expose. Main-owned
Engine/API orchestration now persists the pre-Question continuation, freezes
each operation before execution and atomically materializes its Question
snapshot through `GET /api/v1/tasks/{task_id}/questions`. The durable
post-materialization `SubmissionExecute` path uses the same operation ledger.
Its explicit Answer and Skip intents remain distinct: mode 73 has no evidenced
two-answer Verify encoding, while the donor's `SkipAnswer` route is
implemented; an empty or `Unknown` selected answer is not an acceptable
substitute. An accepted next Question atomically consumes the old session,
persists the new immutable Snapshot/session and returns the Task to Ready for a
new Draft. Definite completion closes the session without a fabricated
artifact and proceeds to the separate fresh verification phase.
