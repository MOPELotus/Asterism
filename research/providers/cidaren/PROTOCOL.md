# Cidaren protocol notes

Audit date: 2026-08-11. These notes describe frozen donor behavior and
synthetic parser targets, not live compatibility.

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
expiry or refresh endpoint. Consequently the first batch supports only
`ImportedToken` plus validation. WeChat OAuth, browser storage takeover,
mitmproxy and system-proxy changes remain deferred Capture work.

The native transport now uses the shared non-redirecting HTTPS client. It sends
the token as a sensitive `UserToken` header, preserves the audited mobile
User-Agent plus derived `Abc`/read `Authorization-V` headers, requires JSON
Content-Type and UTF-8, and caps the account body at 64 KiB. HTTP 401/403 maps
to Authentication, 429 retains a bounded Retry-After, redirects/404 map to
ProtocolDrift and server failures remain ProviderUnavailable. Response bodies
are zeroized after validation.

The Core adapter requests only `ProviderAccessToken` and requires exactly one
unexpired record whose account, reference, purpose, ManualImport acquisition
and ProviderSpecific session kind all match the operation. Missing/stale token
metadata maps to Authentication; storage/key failures remain sanitized
Internal errors. No automatic renewal path exists.

The original historical donor contains a `LoginByWechatCode` exchange. It does
not establish a current clean native bootstrap and must not override the
explicit first-batch authentication decision.

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
`over_time` with a duration-like millisecond value, but Asterism does not infer
a close timestamp until bounded addition is validated against current
fixtures. `release_time`, `start_time`, `over_time` and `time_spent` stay
separate; `time_spent` is not converted to learning seconds before live proof.

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
status, while progress publishes only the bounded percentage and remote state.
`time_spent` remains a raw detail observation and is never converted into
duration seconds. Future question or submission operations must perform the
same fresh release-identity binding before using `task_id`, `course_id` or task
type.

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
bytes, or current `jv=99` AES-GCM data. The private donor captures browser
crypto context and derives a per-session key with HKDF before AES-GCM
authentication. That material is Capture-dependent and is not implemented in
the first non-Capture read-only milestone.

The Provider now isolates a non-Capture decoder for exact audited variants:

```text
jv=0
jv=2_1254
jv=2_9214
jv=2_10232
jv=2_10234
jv=3_1021
```

It bounds encoded and decoded data to 2 MiB, accepts only JSON objects/arrays,
removes only the fixed inserted-byte positions for the declared variant,
zeroizes temporary decoded bytes and rejects unknown versions instead of
guessing indices. Public issue 43 establishes the `2_1254` family, but its
textbook word payload is not retained as a fixture. `jv=99` returns
UnsupportedTask at this boundary; the decoder does not justify advertising
QuestionInventory/QuestionParse or starting a remote attempt.

Question discovery, answer resolution, submission construction, mutation and
fresh verification must remain separate capabilities. The donor's strings
such as “task complete” do not replace an identity-bound post-mutation read.
