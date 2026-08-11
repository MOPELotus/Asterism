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

The original historical donor contains a `LoginByWechatCode` exchange. It does
not establish a current clean native bootstrap and must not override the
explicit first-batch authentication decision.

## Course and class-task inventory

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
a static suffix. Exact request signing belongs to the later native transport;
fixtures never contain the suffix-derived signature.

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
course must agree on the normalized course title or the whole scan fails. An
account with no class tasks yields no course through this evidence path; the
single selected `course_id` in `Student/Main` is insufficient for a titled
Course and is not fabricated into one.

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
```

`release_id` is required, positive and unique in a scan. `task_id` remains a
bounded signed observation because later routes may need the current value.
Every future TaskDetail, question or submission operation must re-list and
select exactly one row by release identity before using `task_id`, `course_id`
or task type.

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

Question discovery, answer resolution, submission construction, mutation and
fresh verification must remain separate capabilities. The donor's strings
such as “task complete” do not replace an identity-bound post-mutation read.
