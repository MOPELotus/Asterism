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

The current implementation covers the deterministic password form encoding,
bounded login-envelope classification and a strict
`https://sso.sflep.com/idsvr/` callback allowlist. Password and ImportedCookie
flow through the common Authentication capability behind injected transport and
stored-session boundaries. The real prelogin/OIDC redirect and Cookie transport
is not implemented or registered yet.

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
injected transports, but no native request is issued yet.

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
