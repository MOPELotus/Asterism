# UAI protocol notes

Audit date: 2026-08-09. These are static donor observations and synthetic
parser coverage, not live compatibility claims.

## Authentication

The two backend donors agree on a JSON Password login:

```text
POST https://sso.unipus.cn/sso/0.1/sso/login
     username, password, remember=true, agreement=true,
     service=https://uai.unipus.cn/home
```

A successful response uses string code `0` and returns distinct `openid` and
`jwt` values. Authenticated UAI requests place the returned JWT in
`Authorization`. Donor code `1506` indicates a slider/captcha state and becomes
`HumanRequired`, not a password retry.

The injected Authentication boundary accepts exact username/password fields or
one manually imported provider session document:

```json
{"openid":"...","jwt":"header.payload.signature"}
```

The document rejects unknown fields, unsafe open IDs and non-JWT shapes. Native
Password success produces username + password + one encrypted
ProviderCompositeSession replacement so openid/JWT cannot be committed or
renewed separately. Debug output redacts both. Injected session validation
resolves an account-bound composite and validates the JWT through the transport;
the concrete Core adapter requests only `ProviderCompositeSession`, verifies
the exact account/reference/purpose/expiry tuple, and accepts only
NativeProviderLogin+Composite or ManualImport+Jwt metadata. Storage/key failures
remain sanitized Internal errors while missing or stale credentials are
Authentication errors. The native transport now uses the shared non-redirecting
HTTPS client, posts the exact JSON login shape, passes JWT directly as a
sensitive `Authorization` header and validates it with a bounded user-info
read. It classifies status before reading, requires JSON media types and valid
UTF-8, caps bodies at 64 KiB and never retains returned user profile fields.

Runtime renewal is limited to an exact account/reference-bound username,
password and ProviderCompositeSession set whose three records all originate
from NativeProviderLogin. Per-account locking and a bounded short-lived cache
collapse concurrent refreshes. A fresh Password exchange must pass user-info
validation before Core compare-and-replaces the complete credential set.
Authentication and every read operation retry at most once. ManualImport+Jwt
sessions, incomplete metadata and stale credential versions cannot renew. JWT
standard `exp` is decoded only as a conservative lifecycle hint after the
native user-info authority accepts the token. Native Password credentials may
be resolved after that hint expires solely for an atomic re-login; imported
sessions still cannot renew. Renewed-session cache entries are evicted at JWT
expiry even when their short cache TTL has not elapsed.

The complete Development Provider factory shares one resolved network policy and
one account-scoped stored-session resolver across Authentication,
CourseInventory, TaskInventory, TaskDetail, TaskProgressRead, DurationRead and
the question/answer/submission transports. The daemon does not register this
entry by default. Its config, environment and CLI opt-ins are independent from
the other development Providers, require a configured SecretStore and retain
Development verification with a startup warning.

## Course resource and tree

```text
GET https://uai.unipus.cn/api/account/user/info
GET https://uai.unipus.cn/api/cmgt/course/getCourseListByStudent
GET https://uai.unipus.cn/api/cmgt/course/getCourseResourceInfoById/{resourceId}
GET https://ucontent.unipus.cn/course/api/course/{courseInstanceId}/default
```

The Course list contains Course rows with nested `courseResourceList` rows. A
CourseResource selects a distinct tutorial instance, so the normalized Course
identity is the bounded resource `id`, not a mutable title, class ID or current
instance route. The detail response must repeat `courseResourceId` and supplies
a fresh `courseInstanceId`; that route is held only in a redacted operation
context.

The native CourseInventory resolves one account-bound composite session and
reads the fixed Course-list route with the raw JWT in a sensitive Authorization
header. It applies status, JSON Content-Type, UTF-8 and 4 MiB response bounds
before the complete parser runs; Course management reads require numeric code
`1` and `success=true`, while ucontent tree/progress/content reads require their
numeric code `0`. No partial inventory or route-only class and instance fields
are returned. Authentication failures can renew one complete native session
and restart the operation once.

The content response stores the actual Course tree as JSON text in outer field
`course`. The nested root contains `units`; audited roles include `unit`,
`section`, `node`, `link` and `group`. Group IDs are the stable task leaves used
by the progress response. Asterism preserves Unit → Section → Node/Micro → Group
as distinct hierarchy and identity facts. `base` is only a bounded task-type
label and does not imply an assessment or execution capability.

Native TaskInventory first reads and parses the selected resource detail,
requiring its `courseResourceId` to match the stable Course identity. Only then
is the fresh `courseInstanceId` encoded as one URL path segment for the tree
read. Both completed bodies are held in one redacted transport result and
parsed all-or-nothing; the instance route is never serialized into a Course or
Task. Authentication renewal restarts the detail/tree pair from the fresh
detail; progress and duration remain independent capabilities over the same
native transport.

TaskDetail never trusts the persisted scan payload as current. It parses the
stable `group:{courseResourceId}:{unitId}:{groupId}` identity, re-reads the
Course list, selects exactly one CourseResource, then re-runs the complete
fresh detail/tree Task inventory and requires exactly one matching Group. A
missing CourseResource or Group becomes RemoteChanged; duplicate or mismatched
normalized identities are protocol drift. The bounded tree facts now preserve
`question_num` beside `base`, and Task fingerprints use an explicit `v1:`
prefix for Core detail validation and future schema migration.

## Progress and duration separation

```text
GET https://ucontent.unipus.cn/course/api/v2/course_progress/
    {courseInstanceId}/{unitId}/{openid}/default

GET https://uai.unipus.cn/api/tla/learningDetail/studyRecord/
    totalAndUnitSituation?id={courseResourceId}&appUserId={appUserId}

GET https://uai.unipus.cn/api/tla/learningDetail/studyRecord/
    unitTaskSituation?nodeId={unitId}&id={courseResourceId}
    &appUserId={appUserId}&ssoId={ssoId}
```

Both backend donors generate the same HS256 `x-annotator-auth-token` from the
account-bound openid and send it beside the raw Authorization JWT. The native
reader recreates that fixed protocol token internally for this GET only; both
headers are sensitive and neither is logged or persisted. A fresh resource
detail is rebound before each progress URL is built.

Normalized Group identity is `group:{resourceId}:{unitId}:{groupId}` so the
per-Unit route never guesses an ancestor or persists `courseInstanceId`. The
response must repeat the Unit and contain the exact Group leaf. A Group maps to
Completed/100 only when `pass`, `pass2` and `perm` are all `1`; every other
valid flag combination remains Unknown with no percentage.

The independent study-record responses contain both `finishProgress` and
`duration`. The frozen MIT donor explicitly documents Course, Unit and nested
Task `duration` as learning seconds and passes the numeric CourseResource ID as
query parameter `id`; it is not a strategy ID. Asterism therefore exposes an
independent DurationRead but does not infer reporting semantics.

Each duration operation resolves the account-bound session, obtains only the
bounded `appUserId` and `ssoId` facts from a fresh user-info response, then
reads `unitTaskSituation` for the exact CourseResource and Unit. The parser
requires exactly one matching Unit, unique node identities and exactly one
matching Group identity with audited `link` or `group` role and an explicit
unsigned duration. Unknown roles, duplicate IDs, missing/negative/overflow
duration and every identity mismatch fail closed. An Authentication failure
restarts the whole user-info + duration pair once after atomic renewal.

The separate progress response can also contain numeric `duration`. Asterism
retains it only as `duration_raw`; it never populates `duration_seconds` from
that field.

The current userscript distributes a requested residence time across visible
Unit/Section/Micro pages, tabs and tasks while keeping the real page active. It
does not expose an explicit portable duration request in the audited source.
Therefore Asterism must not invent a NativeDurationReporter from the timer.
BrowserBridge/Capture work remains deferred, and a future native reporter needs
sanitized network evidence plus fresh duration readback.

DurationRead never creates an Execution, reports time or mutates remote state.
The independently implemented submission route does not report duration. HTTP
success or 100% completion cannot satisfy any future duration-report acceptance
gate.

## Question, answer, submission and verification audit

The frozen backend donors expose four distinct protocol stages:

```text
GET  https://ucontent.unipus.cn/course/api/v3/content/
     {courseInstanceId}/{groupId}/default
GET  https://ucontent.unipus.cn/course/api/v3/answer/
     {courseInstanceId}/{groupId}/default
POST https://ucontent.unipus.cn/course/api/v3/newExploration/submit
GET  https://ucontent.unipus.cn/api/mobile/user_module/
     {courseInstanceId}/{groupId}-{submitVersion}
```

Content and answer responses carry separate `unipus.`-prefixed AES-ECB
ciphertext plus a bounded key suffix; submission returns a version used by the
fresh user-module read. Rate-limit codes `600001` and `600002` are not success.
The donor also branches by `base` and `question_num`, including objective,
preset, oral, subjective, discussion, exit-ticket and upload behavior.

Asterism now implements QuestionInventory/QuestionParse, AnswerResolve,
SubmissionBuild, SubmissionExecute and SubmissionVerify as independent
capabilities. Content and standard-answer reads each refresh and bind the exact
CourseResource/Group route, bound/decrypt their own `unipus.` document and drop
ciphertext plus key material after normalization. The immutable draft preview
contains question structure and answer shape but no selected answer values and
no executable provider payload. Each sanitized Question also carries the exact
stable `group:{courseResourceId}:{unitId}:{groupId}` Task identity; answer
resolution and draft construction reject a snapshot from any other route.

Execution is advertised only when a fresh Group has exactly one question and
one audited simple type: `single-choice`, `multichoice` or `short_answer`. The
execution boundary additionally requires the explicit positive numeric answer-
entry instance ID used by both donors; arbitrary strings remain read-only and
cannot reach the POST. The native body is constructed only inside the execution
boundary. A code-`0`,
version-bearing response becomes an accepted receipt; `600001` and `600002`
become typed retry state without a receipt, and the mutation is not implicitly
repeated. A Network failure from the mutation boundary is likewise returned
after one attempt so Core can retain verify-only recovery authority without
replaying the POST.

Verification requires that accepted receipt and reads only the exact
`{groupId}-{submitVersion}` user-module route after refreshing the Course
instance. The response must bind both the top-level and submit-info Course to
that fresh route, repeat the exact `{groupId}-{submitVersion}` module identity,
bind Group and both version fields, contain exactly one matching question in
submitted state, mark every answer child done, and reproduce the immutable
draft answer exactly. Only then is the question Confirmed. No score, progress
or Task-completion state is inferred. With no receipt the result is
Inconclusive and no version route is guessed or read.
Unsupported or unsafe types still fail closed; empty answers, synthetic uploads
and discussion side effects are not portable defaults. This remains synthetic
offline coverage and is not a live compatibility claim.
