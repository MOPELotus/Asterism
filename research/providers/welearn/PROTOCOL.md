# WELearn protocol notes

Audit date: 2026-08-09. All observations below come from static donor review and
remain unverified against the live service.

## Authentication

The current donor starts with a non-following request to:

```text
GET https://welearn.sflep.com/user/prelogin.aspx
    ?loginret=http%3a%2f%2fwelearn.sflep.com%2fuser%2floginredirect.aspx
```

Its `Location` enters the `sso.sflep.com` OIDC flow. The password exchange posts
form fields `account`, encrypted `pwd`, `ts` and `rturl` to:

```text
POST https://sso.sflep.com/idsvr/account/login
```

The returned redirect must be followed to complete Cookie establishment.
Authentication responses may signal image captcha or SMS verification. Those
states are `HumanRequired`; automated password retry must stop.

The native transport covers deterministic password form encoding, bounded
login-envelope classification and a strict `https://sso.sflep.com/idsvr/`
callback allowlist. It follows at most twelve redirects manually with the shared
non-redirecting client; every hop must remain HTTPS on exactly
`welearn.sflep.com` or `sso.sflep.com`. `returnUrl` must be unique, bounded and
point to the expected OIDC callback shape.

Set-Cookie values are collected in a bounded redacted jar. Only `sflep.com`,
`sso.sflep.com` and `welearn.sflep.com` scopes are accepted; domain and path
matching are checked before each request, and deletion removes any retained
value. The final Cookie is accepted only after the Course-list endpoint returns
a parseable authenticated response. Password and ImportedCookie continue
through the common Authentication capability. Stored Cookie reads now use only
Core's provider-scoped resolver and require an exact account, requested secret
reference, Cookie purpose, supported session kind and unexpired metadata before
the native request.

When a stored Composite session fails specifically as Authentication, renewal
resolves exactly one username, password and Cookie created by native Password
login. It repeats the bounded SSO exchange, validates the new Cookie with a
Course-list read, then submits the complete replacement through Core's atomic
compare-and-replace boundary. Per-account locks and an operation-bound bounded
cache prevent duplicate renewal inside the same operation. Authentication and
inventory retry at most once; a Task scan restarts from its Course page so a
partial Unit/SCO set is never returned. Network, rate-limit, protocol and
ordinary invalid-response failures do not trigger login. ImportedCookie has no
password material and therefore cannot renew. Daemon registration is explicit;
live-account validation remains pending.

## Course and task inventory

```text
GET  /ajax/authCourse.aspx?action=gmc
GET  /student/course_info.aspx?cid={cid}
POST /ajax/StudyStat.aspx
     action=courseunits&cid={cid}&uid={uid}
GET  /ajax/StudyStat.aspx
     action=scoLeaves&cid={cid}&uid={uid}&unitidx={index}&classid={classid}
```

Observed minimum Course fields are `cid`, `name` and `per`. The Course page
contains account/course routing values `uid` and `classid`; they are not stable
Task identity. Unit facts include `unitname` and `visible`. SCO leaf facts include
`id`, `location`, `isvisible`, `iscomplete` and, in the duration donor,
`learntime`.

Stable normalized hierarchy is Course → Unit index → SCO ID. Mutable labels,
visibility, completion and duration must not be used as identity.

The audited `scoLeaves` shape does not expose a trustworthy assessment-nature
field. Every SCO therefore keeps `assessment_class=Unknown`; a Resource module
label or ordinary-looking title is not evidence that an item is Routine, and
cannot be used to bypass the independent formal-assessment policy.

The current runtime boundary requires one Course-list document and, for every
explicit Course scan, one Unit document plus exactly one `scoLeaves` response
per Unit. An empty Unit response is valid; a missing, duplicated, out-of-range
or cross-Unit response rejects the whole scan. Authentication and both inventory
slots can be composed into a registry-consistent Development entry using
injected transports.

Fresh TaskDetail parses only the stable `sco:{cid}:{scoid}` identity, re-lists
the current Courses, requires exactly one matching Course, then re-runs that
Course's complete Unit/SCO scan and requires exactly one matching SCO. It never
reuses persisted `uid`, `classid` or Unit routing facts. Missing Course/SCO
identities are RemoteChanged; duplicate or normalized identity drift is
ProtocolDrift. Task fingerprints use the versioned `v1:{sha256}` grammar
required by the Core detail boundary.

The native inventory adapter now uses the shared non-redirecting HTTP client.
It reads the fixed HTTPS Course endpoint, re-fetches the selected Course page,
extracts one unambiguous `uid`/`classid` pair, posts `courseunits`, then reads
every indexed `scoLeaves` document with the same resolved Cookie. HTTP status,
login redirects/pages, response content type, UTF-8 and body limits are typed
before parser entry. This is native-boundary coverage only; it has not been run
against a live account.

## CMI lifecycle

Observed mutation/read actions on `POST https://welearn.sflep.com/Ajax/SCO.aspx`
include:

```text
getscoinfo_v7
startsco160928
setscoinfo
keepsco_with_getticket_with_updatecmitime
savescoinfo160928
```

The implemented read-only path first refreshes the selected Course page to
resolve its current `uid`, then sends the exact form action and stable IDs:

```text
POST /Ajax/SCO.aspx?uid={uid}
     action=getscoinfo_v7&uid={uid}&cid={cid}&scoid={scoid}
```

The audited response is an outer JSON object whose `comment` string contains a
second JSON document. The nested optional `cmi` object is parsed field by field;
completion, progress and both time strings remain independent. Missing `cmi`
means the donor-observed not-attempted zero-progress state. A non-zero `ret`, a
non-string `comment`, malformed nested JSON or an unsafe scalar fails closed.
The native adapter uses the same bounded authenticated session and one
Authentication-only renewal retry as inventory. It does not call `startsco160928`
when CMI is absent or malformed, because a read capability must never mutate
remote learning state.

The donor CMI object exposes separate `completion_status`, `progress_measure`,
`session_time`, `total_time`, score and success status. Asterism must preserve
these independent facts:

```text
Completion != Progress != Session Time != Total Time
```

The implemented `DurationReport` lifecycle acquires a fresh Course route and
CMI document using one resolved Cookie. If a valid response explicitly has no
`cmi`, it calls `startsco160928` once and re-reads the baseline. Malformed or
ambiguous reads never trigger a start fallback. The post-start read must expose
the complete audited CMI preservation set; a still-absent or partial CMI stops
before any heartbeat or finalization rather than synthesizing defaults. The
baseline preserves bounded `completion_status`, `progress_measure`,
`score.scaled`, `success_status`, `session_time` and `total_time` values.

The lifecycle sends an initial and then periodic
`keepsco_with_getticket_with_updatecmitime` heartbeat, with real elapsed waits
and the preserved session/total observations, before `savescoinfo160928`
finalizes the session. The final form carries the preserved completion,
progress, score and success values; it never uses donor paths that fabricate
completion or scores, and it never calls `setscoinfo`. Start/finalize require
integer `ret=0`; heartbeat accepts only the two donor-observed integer values
`0` and `1`.

After finalization, a fresh CMI read must preserve completion, progress, score
and success status with the same explicit field presence while changing at
least one raw time observation. An absent or partial readback is protocol drift,
not evidence that a default-valued state was preserved. HTTP
success alone is never a verified outcome. Authentication may renew once before
the first mutation. A mutation is never replayed after authentication fails;
only the final read-only verification may renew if the operation has not already
used its single renewal.

Master-owned runtime settings expose platform defaults and account/task
overrides for actual report seconds and heartbeat interval. Provider and account
execution concurrency plus periodic Course/Task scan interval are independently
bounded. Until sanitized live responses establish the remote time grammar and
unit, the parser retains raw time strings and `TaskProgressRead` does not report
seconds; `DurationRead` therefore remains unadvertised.

## Sanitization and routing

- `uid`, `classid`, redirect state, PKCE material and Cookies are route/session
  facts, never persisted normalized metadata.
- Only the audited HTTPS hosts may be contacted; unexpected OIDC redirects fail
  closed as protocol drift.
- Course percentage and SCO duration are remote observations, not proof of a
  successful Asterism execution.
