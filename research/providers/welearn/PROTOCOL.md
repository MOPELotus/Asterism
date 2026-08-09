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

The current runtime boundary requires one Course-list document and, for every
explicit Course scan, one Unit document plus exactly one `scoLeaves` response
per Unit. An empty Unit response is valid; a missing, duplicated, out-of-range
or cross-Unit response rejects the whole scan. Authentication and both inventory
slots can be composed into a registry-consistent Development entry using
injected transports.

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

The donor CMI object exposes separate `completion_status`, `progress_measure`,
`session_time`, `total_time`, score and success status. Asterism must preserve
these independent facts:

```text
Completion != Progress != Session Time != Total Time
```

A future duration implementation must acquire fresh CMI, preserve the existing
completion/progress/score values, start when required, issue bounded heartbeats,
finalize, and then re-read remote CMI. Donor paths which set completed/progress
or fabricate scores without first reading the remote state are excluded.

## Sanitization and routing

- `uid`, `classid`, redirect state, PKCE material and Cookies are route/session
  facts, never persisted normalized metadata.
- Only the audited HTTPS hosts may be contacted; unexpected OIDC redirects fail
  closed as protocol drift.
- Course percentage and SCO duration are remote observations, not proof of a
  successful Asterism execution.
