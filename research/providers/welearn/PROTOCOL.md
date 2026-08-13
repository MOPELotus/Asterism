# WELearn protocol notes

Audit date: 2026-08-11. All observations below come from static donor review and
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
states currently become typed `HumanRequired` and automated password retry must
stop. Capture recipe v4 exposes `AssistedSession`, while recipe v5 separately
records `ExternalBrowserOauth`; both let a local Capture/browser helper submit
the resulting bounded Cookie, which the Provider validates against the
authenticated Course endpoint before Core stores it. The current donor's
browser implementation posts image-captcha `icode` and
`sign`, or SMS `vcode` and `vcToken`, from the interactive SSO page. Those
same-origin manual challenge paths can finish under the current recipe. The
page also exposes dynamically supplied QQ, WeChat and Apple third-party login
URLs. A 2026-08-13 read-only audit of the public SSO bundle and unauthenticated
challenge redirects established the exact first-hop HTTPS origins
`graph.qq.com`, `open.weixin.qq.com` and `appleid.apple.com`; both recipes
allowlist them for top-level navigation while restricting credential reads to
the WELearn origin. Recipe v5 records the external-provider path without
claiming which of QQ/WeChat/Apple was selected because Capture has no evidenced
discriminator. Authenticated callbacks have not been live validated, and no
callback URL is treated as independently exchangeable for a Cookie. The donors
contain no
reliable headless solver protocol, so native Password stops as typed
HumanRequired when challenged instead of guessing requests.

The public prelogin response itself sets anonymous `acw_tc` and
`ASP.NET_SessionId` Cookies for WELearn. Therefore a non-empty origin-wide
Cookie header is not an authentication readiness signal: automatic Capture can
otherwise submit before the user completes SSO. A 2026-08-13 public no-account
check proved an anonymous prelogin session can issue the Course-list request
and receive `200 text/html` login script. Both recipes therefore require the
current loader's exact `GET /ajax/authCourse.aspx?action=gmc` response to be
`200 application/json` while collecting the whole WELearn Cookie header. The
Capture helper enforces this response gate, and native Course parsing remains
the final acceptance authority.
The SMS token takes precedence over the numeric login code: the audited browser
implementation treats `code=0` with a non-empty `extraCheck.vcToken` as a
continuation challenge, not as a redirect-ready success.

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
a parseable authenticated response. Password, ImportedCookie and
Capture-assisted and external-browser-OAuth Cookies continue through the common
Authentication capability. The immutable v4/v5 recipe alternatives both emit
only the final WELearn Cookie; Core freezes one acquisition method and never
combines outputs. Captcha fields, SMS tokens and OAuth state remain browser-local.
Stored Cookie reads now use only
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

The current Fanyuchang donor intentionally leaves both the hidden-SCO and
already-completed skip conditions disabled while building its work list. Thus
`isvisible=false` remains an honest `NotOpen` observation, but it does not erase
the donor-evidenced ResourceExecution, ExecutionVerify or DurationReport
capabilities. Asterism retains the visibility boolean through fresh discovery
and verification; the dispatcher grants Core's NotOpen exception only to the
exact evidenced WELearn action sets and never to Expired or Removed tasks.

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

The current donor's POST form is primary. The YZBRH and Auto_WeLearn donors
use an equivalent GET query for `courseunits`; native inventory retries that
read-only alternate only when POST receives an explicit 404 or 405. It does not
switch method after authentication, rate-limit, network, body or parser errors.

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

The implemented non-mutating TaskProgress path first refreshes the selected Course page to
resolve its current `uid`, then sends the exact form action and stable IDs:

```text
POST /Ajax/SCO.aspx?uid={uid}
     action=getscoinfo_v7&uid={uid}&cid={cid}&scoid={scoid}
```

The audited response is an outer JSON object whose `comment` string contains a
second JSON document. The nested optional `cmi` object is parsed field by field;
completion, progress and both time strings remain independent. Missing `cmi`
means the donor-observed not-attempted zero-progress state. The outer `ret`
must be the integer `0`; a missing, non-integer or non-zero result, a non-string
`comment`, malformed nested JSON or an unsafe scalar fails closed.
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
CMI document using one resolved Cookie. Malformed or ambiguous reads never
trigger a start fallback. Every mode establishes a post-start baseline and
requires the complete audited CMI preservation set; a still-absent or partial
CMI stops before any heartbeat or finalization rather than synthesizing
defaults. The baseline binds bounded `completion_status`, `progress_measure`,
`score.scaled`, `success_status`, `session_time` and `total_time` values.

Three donor wire modes remain separate:

- `preserve_fresh` matches YZBRH: start only when CMI is absent, send one
  immediate keep plus one preserved-time keep after each complete configured
  interval, wait a trailing partial interval without inventing another keep,
  then save the preserved completion/progress/score/success tuple;
- `client_counter` matches current Fanyuchang: always start, send exactly one
  keep per real second with `session_time` and `total_time` progressing from 0,
  and leave completion to the following ResourceExecution step;
- `implicit_server` matches Auto_WeLearn: always start, wait before each
  60-second keep, omit explicit time fields, wait any trailing partial interval,
  then issue the donor's bare save.

The start payload also follows that selected wire mode. Current Fanyuchang
sends `uid/cid/scoid/classid/tid=-1`; YZBRH and Auto_WeLearn send only
`uid/cid/scoid`. All three duration donors use the simple StudyCourse Referer.
Current Fanyuchang addresses `/Ajax/SCO.aspx?uid=...`; the two historical modes
address plain `/Ajax/SCO.aspx` for CMI read, start, keep, save and final read.
These are exact protocol distinctions, not optional-field optimizations.

YZBRH also spaces the baseline read, conditional start retry, first keep and
final save with its fixed two-second request interval. Asterism preserves those
delays around the configured learning duration; they are not counted as
reported duration. Its keep form contains only the two preserved time fields
beyond common identity (unlike current Fanyuchang's additional
`timelimitsec/endcaltime` fields), and its final save writes literal
`status=unknown`. Well-formed integer negative receipts are diagnostic and do
not suppress the donor's next independent write; all acceptance counts are
reported, while only the final fresh CMI preservation/readback proves success.

The client-counter and implicit modes require their evidenced 1-second and
60-second intervals respectively. Current Fanyuchang requires accepted
integer `ret=0` at start and accepts heartbeat `ret=0/1`; the historical modes
record every well-formed integer receipt and continue their donor sequence.
Missing/string/malformed receipts remain ambiguous and stop with no replay.
None of these duration modes calls `setscoinfo`; completion remains a separate
capability.

After finalization, a fresh CMI read must preserve completion, progress, score
and success status with the same explicit field presence while changing at
least one raw time observation. An absent or partial readback is protocol drift,
not evidence that a default-valued state was preserved. HTTP
success alone is never a verified outcome. Authentication may renew once before
the first mutation. A mutation is never replayed after authentication fails;
only the final read-only verification may renew if the operation has not already
used its single renewal.

The shared Engine treats an only-`DurationReport` request as goal-verified when
the Provider returns `verified=true` and a Pending, InProgress or Completed
remote Task state. This does not reinterpret duration as completion. Any
Provider error is an uncertain duration mutation and goes directly to
HumanRequired without retry. An abandoned duration-report execution also goes
to HumanRequired without consulting TaskProgress or re-entering start/keep/save.

The independent `DurationRead` path interprets canonical decimal `total_time`
as seconds because both current donors advance/post those counters against real
one-second waits. An explicit no-CMI state is zero. Empty, signed, fractional,
leading-zero, SCORM-duration or over-ten-year observations fail closed as
protocol drift until sanitized evidence supports another grammar. Generic
TaskProgress deliberately remains free of duration; the dedicated capability
owns this normalization.

Master-owned runtime settings expose platform defaults and account/task
overrides for fixed or donor-observed bounded-random report seconds, heartbeat
interval and wire mode. One-second granularity is accepted within a bounded
1–7,200 second report and 1–90 second heartbeat range: the current donor emits
one heartbeat per second and documents 10–30 second sessions, while the older
duration donor emits a 60-second cadence. Conservative defaults remain 600 and
60 seconds. Random duration selection is derived from Core's immutable
Execution identity plus the bound Task and remote identity: retry/recovery
reproduce the same value, while a later Execution can sample another value.
Provider and account
execution concurrency plus periodic Course/Task scan interval are independently
bounded. Auto_WeLearn exposes 1–100 concurrent workers; both WELearn
concurrency settings therefore accept 1–100 with a default of 1, while Core's
global admission and durable leases remain the final scheduling bounds.

## Completion, progress and score execution

All three pinned donors expose direct SCO execution behavior and agree on this
common sequence:

```text
fresh Course/SCO rebind
→ startsco160928
→ setscoinfo(completion_status=completed, progress_measure=1, score.scaled=selected)
→ savescoinfo160928(progress=100, crate=selected, cstatus=completed)
```

The `setscoinfo.data` envelope has two audited forms. Current Fanyuchang sends
the serialized CMI JSON directly. YZBRH and Auto_WeLearn append the exact
literal `[INTERACTIONINFO]` immediately after that JSON. Runtime setting
`execution.completion_cmi_format` keeps `json` and
`interaction_info_suffix` explicit; the latter does not alter or parse the CMI
body before appending the marker.

Mutation family is also explicit. Exercise completion uses `set_then_save`.
Auto_WeLearn's duration worker instead uses `save_only`, directly sending the
completed/progress-100/score-0 tuple after its duration lifecycle. Both still
issue `startsco160928` and end with the same goal-bound fresh CMI verification.
The native transport prepares any CMI document before the first mutation and
omits it entirely for `save_only`.

The completion donors also pair two different start forms with different
Referers. Current Fanyuchang sends the full
`uid/cid/scoid/classid/tid=-1` payload with the simple StudyCourse Referer.
YZBRH and Auto_WeLearn send only `uid/cid/scoid` with the task-specific
`StudyCourse.aspx?cid=...&classid=...&sco=...` Referer. Runtime setting
`execution.mutation_profile` exposes exactly these two evidenced families,
applies the selected endpoint and Referer to baseline read, start, set, save and
verification alike, and does not synthesize the previously unevidenced
full-payload/task-Referer mixture. Current Fanyuchang uses
`/Ajax/SCO.aspx?uid=...`; YZBRH/Auto use plain `/Ajax/SCO.aspx`.

Current Fanyuchang then unconditionally performs another
`savescoinfo160928(progress=100, crate=100, cstatus=completed)`, even when the
selected score was different or already 100. YZBRH and Auto_WeLearn stop after
the selected-score save. Asterism exposes these as separate `selected_score`
and `current_donor_dual_save_100` execution-sequence settings rather than
silently dropping the current donor's extra mutation. Both end with a fresh
`getscoinfo_v7`; their exact verified goals are respectively selected and 100.

The donors also evidence two distinct CMI time shapes. Ordinary completion in
Fanyuchang, YZBRH and Auto_WeLearn writes `session_time=0` and `total_time=0`.
Current Fanyuchang's duration path instead accumulates both fields during its
heartbeat loop and carries them into the completion `setscoinfo`. Asterism
therefore exposes `zero_time` and `preserve_fresh_time` completion-time modes.
The latter reads the current CMI after the independent duration phase, requires
both bounded scalar fields before starting any mutation, writes them unchanged,
and compares them with the immediate fresh readback. It does not synthesize
missing time values.

The completion-time setting defaults to `auto`. It consults Core's immutable
capability plan without broadening the current step's mutation authority: only
the exact second step of `DurationReport → ResourceExecution` selects
`preserve_fresh_time`; standalone completion selects `zero_time`. An explicit
task setting overrides this inference, including Auto_WeLearn's composite
zero-time profile. Malformed plan/position bindings fail before fresh detail or
transport calls.

This also leaves Auto_WeLearn's separate duration-completion behavior
expressible without a private flow: its duration worker starts and heartbeats,
then saves `completed`, progress 100 and score 0 without `setscoinfo`. Asterism's
durable `DurationReport → ResourceExecution` plan with fixed score 0,
`selected_score` and `zero_time` reaches the same externally observed final CMI
goal, while retaining fresh rediscovery and verification around both phases.

This behavior maps to `ResourceExecution`, rather than Question/Answer/
Submission, because the audited implementations do not inventory questions or
submit individual answers. Their user-facing “exercise accuracy” choice is a
CMI score preset. Asterism implements both a fixed integer score and a bounded
random interval. Auto_WeLearn samples that interval uniformly, while current
Fanyuchang samples a normal distribution centered in the interval with
standard deviation `(maximum - minimum) / 6`, rounds, then clamps. The current
Asterism selector exposes both modes and derives them from Core's immutable
Execution identity plus Task/remote binding. It therefore resamples across new
Executions while reproducing the exact frozen goal during retry and recovery.

The Provider performs a complete fresh TaskDetail rebind before mutation. If
the baseline CMI is already Completed, it skips all mutation and verifies that
fact. Otherwise it builds the bounded CMI document before the first mutation,
uses exact fresh `uid`/`classid` routing, accepts only integer `ret=0` from each
write, emits the configured CMI envelope, performs the configured one- or
two-save sequence, and finally requires
a fresh CMI showing exactly Completed, progress 100 percent and the configured
sequence's final score. Fresh-time mode additionally requires the two CMI time
fields to survive that immediate readback. HTTP acceptance alone is not
success.

No start/setscoinfo/save call is replayed after an ambiguous outcome. The
donors do continue their later independent mutation when a previous endpoint
returns a well-formed, explicit non-success `ret`; Asterism records each such
acceptance boolean and continues the configured write plan, but still requires
the exact fresh CMI goal. A malformed response, transport failure or other
ambiguous outcome stops immediately as HumanRequired. Thus a receipt remains
diagnostic evidence rather than success.
The
goal-bound `ExecutionVerify` implementation receives Core's same frozen
ExecutionRequest, repeats the full TaskDetail identity rebind and calls only a
fresh `getscoinfo_v7`. It recomputes the selected and final scores from the
frozen goal and requires the exact completion/progress/final-score tuple.
Transient verification
reads may be retried by Core, but the mutation path is never entered.

## Batch selection and duration allocation

The three donors do not share one batch algorithm. Their exact audited
orchestration semantics are:

| Donor flow | Selection and membership | Per-child target | Dispatch |
|---|---|---|---|
| Current Fanyuchang completion | one, several or every Unit; every returned SCO is retained, including hidden and already-completed rows | fixed score or one clamped-Gaussian score sampled independently for each SCO | one thread per selected SCO |
| Current Fanyuchang duration | one, several or every Unit; every returned SCO is retained | fixed seconds or one inclusive uniform-range duration sampled independently for each SCO | one thread per selected SCO |
| YZBRH completion | one Unit or every Unit; hidden and already-completed SCOs are skipped | fixed score or one inclusive uniform-range score sampled independently for each remaining SCO | sequential SCO mutations |
| YZBRH duration | one Unit or every Unit; every returned SCO is retained | fixed seconds or one inclusive uniform-range duration sampled independently for each SCO | all SCO coroutines share one wall-clock heartbeat window |
| Auto_WeLearn completion | one or several Units; hidden and already-completed SCOs are skipped | one configured score is applied to every remaining SCO | Units and SCOs are processed sequentially |
| Auto_WeLearn duration | one or several Units; all visible SCOs are first collected into one membership set | sample `actual_minutes = max(1, configured_minutes + uniform(-range,+range))` once, then give every child `floor(actual_minutes * 60 / child_count)` seconds; the remainder is deliberately discarded | bounded thread pool, configured from 1 through 100 |

These are orchestration semantics over the already implemented native SCO
operations, not new WELearn endpoints. In particular, Auto's minutes value is
one selected-range budget rather than a per-Unit value despite the current UI
log text calling it “每单元”; the worker collects every selected Unit before it
samples and divides the budget.

Asterism currently retains Unit index/title/code in each fresh Task observation
and exposes per-Task runtime settings, but Unit is not yet a shared first-class
selection scope and execution creation is single-Task. The complete donor
behavior therefore requires Core/API to freeze the selected Unit/all-task set,
the donor-flow discriminator, selection-time membership and visibility facts,
derive and persist each child target, apply provider/account concurrency, and
recover children independently. The Provider must not create its own task
scheduler or silently recompute the distribution after a crash. Auto's integer
division remainder and the one sampled aggregate budget are frozen attempt
facts; a rescan that adds, removes or changes visibility of SCOs must not
redistribute an already scheduled batch.

## Sanitization and routing

- `uid`, `classid`, redirect state, PKCE material and Cookies are route/session
  facts, never persisted normalized metadata. Fresh Course route values and
  parsed CMI preservation scalars are redacted from Debug output where exposed
  and explicitly zeroized when their operation-local containers are dropped.
- Only the audited HTTPS hosts may be contacted; unexpected OIDC redirects fail
  closed as protocol drift.
- QQ/WeChat/Apple are allowlisted only at the three exact origins established
  by the public SSO redirects. No credential source reads their headers,
  storage or Cookies; only the final WELearn Cookie is emitted.
- Anonymous prelogin Cookies never satisfy Capture readiness: their exact
  Course-list request returns `200 text/html`, while both recipes require `200
  application/json` under the current loader. Native Course parsing validates
  the resulting Cookie again before storage.
- Course percentage and SCO duration are remote observations, not proof of a
  successful Asterism execution.
- A selected score and completion preset are non-secret attempt facts, but the
  route identity and CMI bodies remain operation-local, bounded and redacted.
