# UAI protocol notes

Audit dates: 2026-08-13 and incremental 2026-08-15/2026-08-16. These are static donor observations and synthetic
parser coverage, not live compatibility claims. The recorded donor default
branches, tags and releases were refreshed on this date; unchanged revisions
remain reproducible audit snapshots rather than permanent update ceilings.

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

The injected Authentication boundary accepts exact username/password fields in
a transient `ProviderSpecific` candidate or one imported provider session
document from ManualImport, CaptureTool or BrowserExtension:

```json
{"openid":"...","jwt":"header.payload.signature","school":"optional-u-school"}
```

The document rejects unknown fields, unsafe open IDs and non-JWT shapes. Native
Password success produces username + password + one encrypted
ProviderCompositeSession replacement so openid/JWT cannot be committed or
renewed separately. Provider metadata declares that transient input kind as
well as the persisted `Composite` and `Jwt` kinds, allowing Core to validate
both sides of the replacement without confusing the password candidate for a
stored session. Debug output redacts both. Injected session validation
resolves an account-bound composite and validates the JWT through the transport;
the concrete Core adapter requests only `ProviderCompositeSession`, verifies
the exact account/reference/purpose/expiry tuple, and accepts only
NativeProviderLogin+Composite or one of the three imported authorities paired
with Jwt metadata. Capture recipe v4 uses `AssistedSession` and reads
`u-openid`, the raw `Authorization` value, an optional current-donor `u-school`
header and the Cookie header from one `ucontent.unipus.cn` request snapshot. Its
required outputs are one `ProviderCompositeSession` JSON object with `openid`,
`jwt` and, when observed, `school` fields—not independently commit-able
secrets—plus one purpose-bound `ProviderCookie`. Ordered composite-source
alternatives preserve the school header when present without making it a
requirement for accounts whose authenticated request omits it.
The declarative recipe starts at `https://ucontent.unipus.cn/`, polls at 500 ms,
navigates only the `ucontent`/`ipub` HTTPS origins observed by the browser donor
and reads request material only from `ucontent`. Browser
request observation and recipe execution remain a shared Capture-helper
integration responsibility; metadata and the actual recipe are
registry-validated together, so either one cannot claim the contract alone.
Storage/key failures remain sanitized Internal errors while missing or stale
credentials are Authentication errors. The native transport now uses the shared non-redirecting
HTTPS client, posts the exact JSON login shape, passes JWT directly as a
sensitive `Authorization` header and validates it with a bounded user-info
read. It classifies status before reading, requires JSON media types and valid
UTF-8, caps bodies at 64 KiB and never retains returned user profile fields.

Runtime renewal is limited to an exact account/reference-bound username,
password and ProviderCompositeSession set whose three records all originate
from NativeProviderLogin. Per-account locking and a bounded short-lived cache
collapse concurrent refreshes. A fresh Password exchange must pass user-info
validation before Core compare-and-replaces the complete credential set.
Authentication and every read operation retry at most once. Every imported Jwt
session, incomplete metadata and stale credential version cannot renew. JWT
standard `exp` is decoded only as a conservative lifecycle hint after the
native user-info authority accepts the token. Native Password credentials may
be resolved after that hint expires solely for an atomic re-login; imported
sessions still cannot renew. Renewed-session cache entries are evicted at JWT
expiry even when their short cache TTL has not elapsed.

The complete Development Provider factory shares one resolved network policy and
one account-scoped stored-session resolver across Authentication,
CourseInventory, TaskInventory, TaskDetail, TaskProgressRead, DurationRead and
the question/answer/submission transports. Its versioned Master-owned runtime
schema v3 defaults both Provider and account execution to serial admission,
requests a 30-minute account scan interval, and exposes bounded page-residence
seconds, optional video playback, pure-study `marker|placeholder` and
direct-discussion `marker|placeholder` mode at Provider, account and Task scope. Core
resolves and freezes those values; the current BrowserBridge Core Gap must pass
that immutable snapshot into the eventual rendered execution rather than read
mutable settings mid-run. Native mutation paths already reject snapshots that
do not match the complete Provider schema. The daemon does not register this
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

The 2026-08-15 Rust donor delta adds only a local display-name heuristic over
the opaque Course instance string. It is not protocol evidence for a new title
field and can fabricate `unknown` labels. Asterism therefore continues to use
the bounded Course and CourseResource `name` fields returned by the management
API and never derives a user-facing title from `courseInstanceId`.

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
read. It extracts the exact unique Unit set from that bounded tree and performs
one signed progress read for every Unit through the same fresh Course instance.
The complete detail/tree/progress set is held in one redacted transport result
and parsed all-or-nothing; a missing, extra or mismatched Unit document fails
closed, and the instance route is never serialized into a Course or Task.
Authentication renewal restarts the complete inventory from fresh detail;
DurationRead remains an independent capability over the same native transport.

TaskDetail never trusts the persisted scan payload as current. It parses the
stable `group:{courseResourceId}:{unitId}:{groupId}` identity, re-reads the
Course list, selects exactly one CourseResource, then re-runs the complete
fresh detail/tree/every-Unit-progress Task inventory and requires exactly one
matching Group. A
missing CourseResource or Group becomes RemoteChanged; duplicate or mismatched
normalized identities are protocol drift. The bounded tree facts now preserve
`question_num` beside `base`, retain positional `base` tokens including
repeats instead of treating them as a set, and use an explicit `v1:` Task
fingerprint prefix for Core detail validation and future schema migration.

## Progress and duration separation

```text
GET https://ucontent.unipus.cn/course/api/v2/course_progress/
    {courseInstanceId}/{openid}/default

GET https://ucontent.unipus.cn/course/api/v2/course_progress/
    {courseInstanceId}/{unitId}/{openid}/default

GET https://uai.unipus.cn/api/tla/learningDetail/studyRecord/
    totalAndUnitSituation?id={courseResourceId}&appUserId={appUserId}

GET https://uai.unipus.cn/api/tla/learningDetail/studyRecord/
    unitTaskSituation?nodeId={unitId}&id={courseResourceId}
    &appUserId={appUserId}&ssoId={ssoId}
```

The current Rust donor reads the Course-level route before every Unit route,
uses its `rt.units` keys as the current Unit inventory and takes
`rt.publish_version` from the same response for submission judge metadata.
Asterism ports that read as a separate bounded, redacted and zeroizing document.
Its Unit set must equal the fresh tree's complete top-level Unit set and its
positive numeric or numeric-string publish version must equal any copy in the
tree; a tree that omits the version is filled from this independent current
snapshot. Every Course-level Unit must contain a typed strategy object, whose
required/minimum-score/window/statistic facts remain distinct from the Group
leaf strategy below. Missing/extra Units or conflicting versions are protocol
drift, so the Provider never silently builds a submission from a stale tree.

Both backend donors generate the same HS256 `x-annotator-auth-token` from the
account-bound openid and send it beside the raw Authorization JWT. The native
reader recreates that fixed protocol token internally for this GET only; both
headers are sensitive and neither is logged or persisted. A fresh resource
detail is rebound before each progress URL is built.

Normalized Group identity is `group:{resourceId}:{unitId}:{groupId}` so the
per-Unit route never guesses an ancestor or persists `courseInstanceId`. The
native response must repeat the session openid as `open_id`, the fresh Course
instance as `tutorialId` and the Unit route as `unit_id`; these values are
compared inside the native boundary and are never serialized. Its optional
positive numeric or numeric-string `publish_version` must agree with the
Course/tree snapshot and can fill a missing legacy tree copy. The response must
also contain the exact Group leaf. A Group maps to
Completed/100 only when `pass`, `pass2` and `perm` are all `1`; every other
valid flag combination remains Unknown with no percentage.

The current Rust donor's progress leaf additionally exposes `strategies` with
`required`, `min_score_pct`, `start_time`, `end_time` and
`statistic_mode_out`, while the MIT donor's execution policy independently
checks the leaf start/end window before running a Task. Asterism bounds minimum
score to `0..=100`, requires booleans for the two flags and accepts only
nonnegative epoch seconds. The donor convention treats either zero endpoint as
an unbounded window; when both are nonzero they must satisfy `start < end`, and
a mutation is eligible only under the donor's strict `start < now < end` rule.
TaskInventory retains those facts, `tab_type`, the resulting `opens_at` /
`closes_at` and completion state in the enriched fingerprint. Missing strategy
objects preserve the current donor's serde defaults rather than inventing a
window.

The independent study-record responses contain both `finishProgress` and
`duration`. The frozen MIT donor explicitly documents Course, Unit and nested
Task `duration` as learning seconds and passes the numeric CourseResource ID as
query parameter `id`; it is not a strategy ID. Exact Task nodes additionally
carry optional `required`, `scoreTaskFlag` and `taskQuesTotalScore`. Asterism
therefore retains one Provider-private Task study record and exposes only its
seconds through independent DurationRead; completion, policy and scoring facts
are not forced into that shared semantic.

Each duration operation resolves the account-bound session, obtains only the
bounded `appUserId` and `ssoId` facts from a fresh user-info response, then
reads `unitTaskSituation` for the exact CourseResource and Unit. The parser
requires exactly one matching Unit, unique node identities and exactly one
matching Group identity with audited `link` or `group` role and an explicit
unsigned duration. The resulting immutable study-record owner retains the
validated CourseResource route, Unit and Group as both separate components and
the complete `group:{resourceId}:{unitId}:{groupId}` Task identity; the
CourseResource no longer disappears after the response parser returns. The
exact Group record separately preserves a finite
`finishProgress` in `0..=100`, optional boolean required/score-task flags and a
bounded unsigned optional Question total score. Unknown roles, duplicate IDs,
missing/negative/overflow duration, malformed optional facts and every identity
mismatch fail closed. An Authentication failure restarts the whole user-info +
study-record pair once after atomic renewal.

`totalAndUnitSituation` is also a first-class donor read rather than merely an
internal skip optimization. The Provider-private native transport first reads
fresh bounded user info, then calls the exact CourseResource/app-user route and
binds the response's `value.user.appUserId` back to that request. Its strict
parser preserves the Course `totalDetail` and unique `role=unit` rows as
separate aggregates: finish progress and score are finite percentages in
`0..=100`, duration is an unsigned donor-documented second count, and each Unit
retains its required flag plus bounded ID/caption/name. Duplicate IDs, foreign
users, unknown roles and malformed metrics fail closed; real names, student
codes, images and route paths are dropped and the response owner is zeroized.
The current shared API has only Task-scoped progress/duration, so exposing this
Course/Unit snapshot remains an accepted independent Core capability gap rather
than a lossy mapping.

The MIT donor also performs an independent required-policy read before choosing
work:

```text
POST https://uai.unipus.cn/api/tla/courseStudyStrategy/detail
{"id": freshStrategyId, "courseResourceId": stableCourseResourceId}
```

The native transport first refreshes Course-resource detail and obtains its
positive strategy ID; neither the strategy ID nor the Course instance is taken
from a persisted Task. The bounded response must repeat that CourseResource and
strategy on both the Course policy and every Unit strategy. The parser retains
binary Unit/inner-Unit unlock modes, bounded scoring mode and the Course window;
each uniquely sorted Unit retains bounded ID/caption/name, decimal-string pass
score in `0..=100`, numeric-string score type, an ordered unique required-Task
list, `requireNodeType=task`, and its own millisecond window. Either zero
endpoint means unbounded; positive endpoints must be ordered. Personal Course
metadata and instance/resource routes are discarded. This policy is kept
separate from current-Rust-donor per-leaf `strategies.required`: both are real
observations with different scope, and reconciliation belongs in the future
shared Course/Unit policy capability rather than silently overwriting one.

The separate progress response can also contain numeric `duration`. Asterism
retains it only as `duration_raw`; it never populates `duration_seconds` from
that field.

The current userscript distributes a requested residence time across visible
Unit/Section/Micro pages, tabs and tasks while keeping the real page active. It
does not expose an explicit portable duration request in the audited source.
Therefore Asterism must not invent a NativeDurationReporter from the timer.
The required implementation is the donor-observed BrowserBridge path. The
frozen `5.2.14` donor supplies the following concrete action evidence:

- run only on rendered `https://ucontent.unipus.cn/*` and
  `https://ipub.unipus.cn/*` pages, including the latter as an iframe;
- discover bounded visible leaf paths from the legacy `pc-slider` tree, Ant
  Tree/Menu ARIA hierarchy, nested `role=menu` hierarchy or current `u3menu`
  courseware list, while retaining Unit/Section/Micro labels;
- click the selected Micro leaf, then visible `.pc-header-tabs-container`
  tabs (or `#header .TabsBox a.topTab`) and `.pc-header-tasks-row .pc-task`
  children in DOM order;
- dispatch mouseover/mousedown/mouseup/click only to the selected allowlisted
  element, scroll it into view, and dismiss the known confirmation modal
  families before and during residence;
- divide the requested total budget over remaining Micros, then Tabs and Tasks;
  pause stops countdown, resume continues it, and a changed total/start index
  restarts from a freshly selected bounded Micro without counting paused time;
- optionally locate the current rendered `video`/`video.vjs-tech`, attempt
  muted playback, restore sound after success, use Video.js controls only as
  fallback, and check once per second until end/removal or the 30-minute donor
  ceiling; active video time suspends the ordinary page countdown;
- for the `ipub` iframe, scan after DOM readiness and mutation, serialize only
  bounded labels plus an element handle, and exchange SCAN/CLICK/result events
  with the owning top-level page.

The donor's outer unit is one Course run rather than one backend Group. It
keeps the rendered menu in source order, applies `menuList.slice(startIdx)`,
then computes `perStepTime = totalSeconds / remainingMicros`. Within each
Micro it divides again by current Tab count and by each current Tab's Task
count; JavaScript `Math.round` is applied only when the final wait begins. A
restart keeps the same total budget but rebuilds remaining jobs from the newly
selected menu ordinal.

Asterism's Task parser therefore preserves bounded nested tree traversal order
instead of sorting Groups by remote ID. The Provider-local immutable
`UaiCourseResidenceBatchPlan` groups only contiguous Tasks with the same
Unit/Section/Micro identity, freezes all ordered Group IDs/types/counts plus
the Course publish version, and rejects a repeated non-contiguous Micro. A
direct Group outside a named Micro becomes its own stable browser leaf. The
plan freezes resolved total seconds/video behavior and retains the budget as
an exact positive rational `totalSeconds / selectedMicros`; bounded runtime
Tab/Task counts are applied before positive half-up rounding. It never creates
an Execution or reports duration. Restart requires a target obtained from the
same plan, binding both ordinal and Micro digest, and changes only start and
plan digest while retaining the full membership digest and total budget.
Fresh membership validation deliberately ignores progress/completion changes
but rejects Course, publish-version, hierarchy, label, Group shape or order
drift. A separate Provider-private child-plan value freezes child and batch
plan versions, Course identity/publish version, an explicit caller-supplied
runtime settings/profile digest, one owner Task inside the selected start
Micro, the complete ordered Task identity/fingerprint sequence and all
membership/start/plan digests. Its strict JSON is bounded to 8 MiB, rejects
unknown fields, revalidates during deserialization and redacts identities and
fingerprints from Debug. Recovery reconstructs the batch from fresh
Course/Task/settings inputs and compares the complete typed value; foreign or
reordered inventory, a changed Task fingerprint, stale settings or another
profile digest fails closed before cursor construction. The Provider does not
guess Core profile IDs or revisions.

Directly placing all 8192 bounded Task identities/fingerprints in Core's
64 KiB `ProviderExecutionPlanArtifact` is not safe. UAI therefore projects the
full child into namespaced type `uai.course-residence-child-plan.v1`: bounded
credential-free JSON retains child/batch versions, Course/publish/profile and
owner/start identities, Task count, all batch/start digests, a domain-separated
ordered-Task digest and a separate digest of the canonical complete child
JSON. The raw child remains Provider-owned and its 8 MiB bound covers the
maximum donor batch. Artifact recovery first rejects a foreign Provider/type
or unknown payload field, then rebuilds the complete child and batch from
fresh Course/Tasks/settings/profile inputs and requires the entire compact
projection to match. The hashes cannot independently authorize a cursor and
no list is truncated to satisfy the shared size bound.

The Provider's version-4 accumulated
cursor can now move into a bounded, zeroizing `SecretValue` artifact with a
stable SHA-256 digest. On
recovery it rebinds the full batch membership and restart-sensitive plan
digests, the exact serialized Browser residence plan and the exact next
command, while strict decoding rejects unknown fields and malformed or
oversized bytes. Core can persist the compact credential-free artifact, but
still needs to attach UAI planning to scheduling and connect the shared browser
executor before this plan is executable.

The donor's `ipub` iframe side alone receives wildcard `UAI_CMD`
`SCAN`/`CLICK`/`PING`; the `ucontent` top page consumes its menu/click/PONG
events and directly performs the main-page Menu/Tab/Task/residence/video DOM
actions. The top page locates that child through four ordered selectors:
`#ipublish-pc-book-easy-iframe`, `iframe.ipublish-pc-iframe-container`,
`iframe[id*="iframe"]`, then generic `iframe`. Initial iframe scanning uses a
MutationObserver bounded to 30 seconds plus at most 20 retries at 1.5-second
intervals. The donor does not validate sender origin. That is behavior
evidence, not an acceptable Asterism security contract: Core must bind every
message to the current browser session nonce, expected frame/window and the
two exact allowed origins. In particular, the generic final selector is
accepted only if transport independently observes the selected frame at exact
`ipub` origin. Arbitrary replacement selectors, wildcard receivers and
donor-generated script are not portable inputs. DOM polls, menu rows, action
counts, residence budgets, video ceilings and popup retries must all be
bounded and cancellation-aware.

The Provider issues a freshly Task-bound BrowserSessionSpec restricted to
`ucontent.unipus.cn` and `ipub.unipus.cn`. Session policy v2 now carries the
exact credential-free HTTPS `browser_start_url_from_detail` result from that
same fresh Group read; its `ucontent` origin must be one of those exact allowed
origins, and its only query fact is the fresh public CourseResource `cid`.
Cookie/JWT/openid material remains isolated session state and never enters the
route. UAI requests no `BrowserBridgeReadSource`; this DOM handler checkpoint
does not gain cookies, storage or any other browser-state read authority. The
Provider independently re-reads the exact Group detail before
creating a versioned private residence plan. Plan v2 binds
the normalized Unit/Section/Micro/Task labels and freezes the four audited discovery families, exact current
iframe/Tab/Task/popup/video selector sets, the exact iframe scan timing above,
a 2048-Micro/64-Tab/128-Task ceiling, bounded popup retries, 3-second DOM polls, the 30-minute donor video ceiling,
the master-resolved residence/video settings snapshot, and mandatory
session-nonce/frame/exact-origin message binding. Validation requires exact
ordered selector equality and exact timing constants, so a modified serialized
plan cannot widen the helper's DOM authority. The Provider result parser
then requires the same plan version, session nonce, frame, allowed origin and
Task; bounds observed residence/video time and Micro/Tab/Task counts; rejects
video activity when disabled; and marks every valid observation as still
requiring fresh DurationRead. The donor's wildcard `UAI_CMD` channel is
represented as a typed, sequence-correlated `SCAN`/`CLICK`/`PING` command and
menu/click/`PONG` event protocol. Donor pause/resume and changed-start-index
restart controls are represented as a separate typed residence-control command;
they retain the exact Task handle and frozen budget, and restart accepts only a
bounded Micro ordinal from the fresh scan. The transport-observed origin must equal both
the exact allowlisted plan origin and envelope origin, nonce/frame/Task must
match, menu labels and cardinality are bounded, menu ordinals must be complete
and ordered. Exactly one menu entry must match the plan's fresh hierarchy;
missing or duplicate matches fail closed, and `CLICK` carries only that
authorized opaque session-derived handle, never a browser-supplied CSS
selector or DOM path. Top-page Tab/Task enumeration uses a separate typed scope
over the exact frozen selector families; its entries are ordered, bounded and
carry binding-derived opaque handles. Tab traversal accepts only those snapshot
handles, and a Task click requires exactly one title equal to the freshly
normalized Group target within the current Tab. Only that authorized Task
wrapper can construct the final residence action; the action copies the frozen
residence/video settings exactly, and the final result must repeat both its
sequence and target handle. The final observation permits at most that one Micro and
one target Task per processed Tab. Target navigation, action dispatch,
pause/recovery, browser session injection and fresh DurationRead acceptance are
an active shared Core integration item.

The helper-side pre-dispatch boundary now consumes Core's zeroizing command
`SecretValue` plus its digest, the exact `BrowserBridgeSessionId`, actual
transport origin/frame and u64 sequence. Before returning any action it checks
the 64 KiB bound and SHA-256, requires exact JSON keys for the envelope and
each tagged action, enforces wire revision 2, derives the only accepted nonce
from the Core session ID, compares actual allowlisted origin/frame and converts
the sequence to a nonzero u32. It also bounds the Group identity, opaque
menu/page handles and residence seconds. The secret owner is dropped before
the safe projection is returned. The resulting enum distinguishes only
`ScanMenu`, `ClickMenu`, scoped `ScanPage`, `ClickTab`, `ClickTask`,
`Residence`, `ResidenceControl` and `Ping`. Its separate zero-sized read-only DOM profile exposes
only the Provider-compiled discovery families, selector sets, cardinalities,
popup/video ceilings and iframe/DOM timings. A command cannot provide a CSS
selector or script; unknown `selector`/`script` fields fail strict schema
validation even on unit actions. `ResidenceControl` projects only a bounded
Task handle, exact active leaf budget and typed pause/resume/restart value into
a selector-free helper-runtime recipe, then accepts only a matching control
acknowledgement whose active seconds do not exceed that budget. When an active
`ResidenceTarget` is available, the control command is derived from that exact
validated session/frame/Task command and copies its rational leaf seconds; it
cannot widen a leaf back to the Course-level plan budget. A non-residence or
foreign active command fails before a control envelope exists. The Provider
can persist the separately issued control's ordinary command artifact and
exchange, fresh-rebind that artifact after recovery, validate the independently
stored raw-result digest and reduce it to a redacted typed acknowledgement plus
completed exchange. That acknowledgement still does not mutate the accumulated
cursor or cancel the active residence exchange; Core must define that
active-exchange cancellation/recovery ownership. This boundary decodes and
projects only; it performs no DOM operation and accepts no helper echo as
authority.

That projection now compiles into a second closed typed value before Capture
interprets any DOM work. `ScanMenu` carries the donor's exact ordered 12-root
container search and four immutable family descriptors: legacy `pc-slider`
Unit/Section/Micro rows, Ant Tree/Menu rows with ARIA-level-then-visible-indent
hierarchy and `aria-expanded`/leaf-class tests, terminal nested
`ul/li[role=menu|menuitem]` hierarchy, and current `u3menu` Unit/courseware
leaves. Each descriptor separately freezes its row, hierarchy-text and
clickable-leaf selectors plus the ordered `title`/`innerText`/`textContent`
fallbacks it actually uses. It also retains the four iframe selectors and
30-second/1.5-second/20-retry scan bounds.

Scoped `ScanPage` recipes freeze both exact Tab selectors or the one exact Task
selector, per-selector text fields, the Tab ARIA/closest-parent/self active
tests and the Task `active`/`pc-task-active`/`current` tests. Each click recipe
contains only the opaque handle copied from the validated projection and the
fixed centered-scroll then `mouseover`, `mousedown`, `mouseup`, `click`
sequence. Residence contains only the projected Task handle/budget/video flag,
five exact popup selectors, two exact video selectors, the 3-second DOM poll,
donor 1-second video poll, 16-popup and 30-minute video ceilings. Residence
control has no selectors and carries only its projected handle, budget and
closed control enum; `Ping` has no DOM facts. Every field is private and has no raw JSON, script or caller-selector
constructor; validation compares all constants, order and bounds exactly, and
Debug redacts DOM, handles and budgets. Recipe compilation still executes no
DOM. Projecting a control recipe still grants no accumulated-cursor transition
or exchange-cancellation authority.

The inverse helper boundary also remains typed. The validated projection keeps
its session/origin/frame/Task/sequence internally but redacts all five from
Debug. Callers can submit only bounded `MenuScanned`, `PageScanned`,
`ClickAcknowledged`, `Residence`, `ResidenceControl` or `Pong` observations. Menu/Page inputs carry
ordered ordinals and rendered labels only; they cannot supply handles. UAI
reconstructs each opaque handle from the retained binding, rejects missing or
reordered ordinals and applies the 2048-Micro/64-Tab/128-Task and 512-byte label
bounds. Click/Ping observations carry no echoed binding. Residence control
accepts only the observed active seconds and reconstructs the exact Task handle
and control from the projection. Residence accepts only
bounded actual active/video/count/cancel/last-label facts and must remain under
the projected action budget and video policy. Only the matching command action
can encode each observation.

Successful encoding uses the existing exact `UaiBrowserEventEnvelope` or
`UaiBrowserResidenceResult`, returns `uai.browser.event` or
`uai.browser.residence.result`, and moves canonical JSON into a zeroizing
`SecretValue` with its SHA-256 digest. Existing Provider parsers validate those
documents unchanged against the original fresh plan/command. The event owner
is bounded to 8 MiB so the already-audited 2048 rows with maximum rendered
labels remain representable; residence stays at 64 KiB. Raw JSON,
selector/script fields and caller-supplied bindings are not encoder inputs, and
the boundary performs no DOM operation.

Core's durable exchange ledger now receives only the stable message types
`uai.browser.command`, `uai.browser.event` and
`uai.browser.residence.result`, plus SHA-256 digests of the validated typed
command or bounded raw result. UAI command/event/residence JSON, browser labels,
session nonce and opaque handles remain outside ordinary Domain storage. Before
dispatch, the exact bounded command can be moved into a redacted `SecretValue`
artifact whose digest is the ledger command digest. After process recovery the
Provider decodes that artifact only when its bytes still match the issued
digest, its fresh plan remains valid, and its Core session, Task and sequence
match. Issuance returns the typed dispatch command, encrypted artifact and
Core exchange together, preventing a caller from persisting metadata for a
different payload. On recovery, origin and frame are restored from the
digest-bound artifact and validated against the fresh plan; they are not
selected by helper output. The Provider still compares the independent
transport-observed result origin before completing the cloned exchange. Raw
intermediate events and terminal residence results therefore enter separate
bounded zeroizing owners; each redacts its nonce/handles/labels, hashes the
exact raw helper document and only then exposes typed parsing against the
restored command. Core's decrypted result-inbox `SecretValue` now moves
directly into those owners: bounds and UTF-8 are checked over the borrowed
secret bytes, and digest/parse borrow the same zeroizing buffer without an
ordinary plaintext `String` copy. Existing in-memory String callers transfer
their allocation through `into_bytes` into the same secret owner. A consuming
inbox owner additionally requires Core's raw-result metadata to describe the
same issued UAI exchange: exact event/residence type, session, sequence,
`received_at >= issued_at` and SHA-256 of those same secret bytes. Only that
owner's trusted receive time can timestamp Provider completion. Both
completion paths fresh-rebind the exact Task and frozen
runtime settings. Intermediate events complete as `uai.browser.event`; only a
valid `ResidenceTarget` can complete as `uai.browser.residence.result`, and
that terminal receipt still requires independent fresh DurationRead. Either
digest remains replay/correlation metadata, not acceptance evidence.

The declared shared result disposition and closed result-type sets now have one
consuming Provider-private adapter instead of leaving callers to choose those
two parsers and cursor transitions independently. UAI declares no credential
result, exactly `[uai.browser.event]` as `Intermediate` and exactly
`[uai.browser.residence.result]` as `ExecutionTerminal`; unknown, command,
credential or near-match types fail before fresh reads. The registry-visible
sets, classifier and strict inbox share that exact mapping. The selected inbox
must match the issued exchange's Core
session, sequence, receive time and exact secret-byte digest. Only then does
UAI fresh-rebuild the Task/settings Browser plan, digest-decode the persisted
command, digest-decode and batch/plan/command-rebind the independent cursor,
parse the typed document against the independently observed origin and compare
the recovered cursor stage.

An Intermediate result can invoke only the transition associated with
`ScanningMenu`, `ClickingMenu`, `ScanningTabs`, `ClickingTab`, `ScanningTasks`
or `ClickingTask`; the consumed parsed event is reduced to the completed Core
exchange plus one immutable next cursor/command. A residence result must bind
the `Residing` stage and full leaf accounting. Its consumed parsed result is
reduced to the completed Core exchange plus a non-resumable immutable
`UaiBrowserResidenceCheckpoint`; there is no next command and fresh
DurationRead remains explicit. Debug for the classifier and both adaptations
is redacted.

UAI now implements the shared consuming workflow callback. Core's compact
encrypted workflow plan carries the same bounded child-plan JSON projection,
not the full Course batch. The callback checks its exact type/schema and frozen
profile identity, then uses Provider-owned CourseInventory and TaskInventory
transports to fresh-read the complete Course and ordered Tasks; rebuilding must
reproduce every child, membership, start and batch digest under the callback's
frozen settings. The runtime sidecar and command/result artifacts then enter
the existing persisted cursor-result adapter. Core's runtime origin/frame must
also equal the recovered command rather than merely passing generic syntax.

For Intermediate, UAI issues the returned immutable cursor/command at the
current result's trusted receive time, derives a new exact command artifact and
`uai.browser.cursor.v4` sidecar, and returns them only through the shared
contiguous-next constructor. For ExecutionTerminal, UAI calls the independently
injected DurationRead transport with the non-resumable checkpoint before using
the shared terminal constructor. It reports the exact fresh duration and
observation time with `RemoteState::Unknown` and no percent, preserving the
fact without inventing completion from residence or the study-record route.
Core already freezes workflow context and atomically stores callback outcomes;
the remaining exact shared gap is the Engine/daemon result processor that
claims an eligible UAI inbox and invokes this callback.

The accumulated residence cursor is a separate encrypted artifact from the
issued command and raw result owners. It contains the bounded snapshots,
counts, remaining active budget and prior-result authority needed to construct
one exact next command. Its ordinary durable metadata is only the artifact
digest; decode verifies those bytes before fresh batch, Browser plan and exact
next-command rebinding, so neither another valid session command nor a changed
runtime plan can inherit the cursor after recovery.

Cursor-aware issuance now returns the typed dispatch command, its command
artifact, the independent cursor artifact and the matching durable exchange
as one Provider owner. The cursor also requires its fresh Unit/Section/Micro
and Task titles to agree with the Course batch, not merely the Group ID. On
recovery the persisted exchange first selects the exact command artifact, then
the separately persisted cursor digest selects and rebinds the accumulated
cursor. Even another valid command issued under the same Core session and
sequence cannot be paired with that cursor.

Core's generic encrypted runtime-state sidecar now supplies that atomic
persistence. The Provider consuming handoff moves both zeroizing artifacts
without copying their plaintext and derives the sidecar metadata only from the
issued exchange and cursor: exact session, sequence, `stored_at=issued_at`,
cursor digest and stable type `uai.browser.cursor.v4`. Strict consuming
recovery rejects a missing/foreign state type, changed command or cursor bytes,
or any session/sequence/time mismatch before fresh Provider reads; it then
performs the existing command-first, cursor-second fresh rebind. The sidecar is
never dispatched to the helper. It stores the accumulated cursor, not the
complete immutable Course batch. The bounded child-plan value now owns the
frozen start/settings-profile/ordered-fingerprint authority; Core persists its
compact artifact projection and must still supply complete fresh ordered
settings and that encrypted projection. UAI itself re-reads complete fresh
Course/Task inventory and recovers the embedded profile identity to rebuild the
exact `UaiCourseResidenceBatchPlan` before cursor parsing.

The first cursor transition is now executable at the Provider boundary. A
completed `ScanMenu` exchange must retain the same Core session, sequence,
command type/digest and raw `uai.browser.event` result digest as the cursor's
issued command. Only its already validated `MenuList` can select the unique
fresh Unit/Section/Micro target. The Provider then returns a new immutable
cursor plus sequence-next `ClickMenu`; the old cursor remains unchanged, and
the new cursor binds the prior raw result sequence/digest and exact click
command digest before it can be encrypted or issued. Replacing the completed
exchange command digest fails before advancing. An accepted completed
`ClickMenu` then replaces the prior-result authority with its own exact raw
digest and returns sequence-next `ScanPage(Tab)` plus an immutable
`ScanningTabs` cursor. A rejected/foreign click cannot advance. The completed
Tab scan freezes its validated ordered snapshot. A non-empty snapshot selects
only ordinal zero for sequence-next `ClickTab`, while an empty Tab list moves
directly to `ScanPage(Task)` without inventing a click. An accepted Tab click
retains the frozen snapshot and current/next ordinals, does not count the Tab
as processed before its Tasks run, and advances to sequence-next
`ScanPage(Task)`. The completed Task scan freezes its validated ordered
snapshot. Exactly one fresh target title authorizes `ClickTask`; a missing
target advances to the next handle from the frozen Tab snapshot, clears the
old Task snapshot and counts the inspected Tab once. Exhausting every frozen
Tab without the target fails `RemoteChanged`.
After the exact Task click is accepted, the Provider computes the residence
leaf from the Course batch's unchanged rational Micro share and the frozen
runtime Tab/Task snapshot cardinalities. It rounds only once through
`rounded_leaf_seconds`; a two-Tab/two-Task page across three selected Micros
therefore receives 100 seconds from a 1,200-second total, while a direct
two-Task page receives 200 seconds. The resulting sequence-next
`ResidenceTarget` and `Residing` cursor bind that exact leaf budget. Ordinary
command issuance continues to require the full plan budget, so a smaller leaf
command is dispatchable only through cursor-aware issuance and its fresh batch
revalidation.

An exact completed residence exchange can now produce a non-replayable
Provider-local terminal checkpoint. The exchange must repeat the cursor's Core
session, sequence and command digest and retain the exact raw residence-result
digest. The observation must be non-cancelled, report the complete leaf active
seconds and agree with the cursor's Micro/Tab/Task counts. Checked accounting
subtracts that leaf from remaining active budget, adds bounded video time and
records the completed command/result identities. The checkpoint carries no
next command and cannot be encoded as a resumable cursor; it still explicitly
requires fresh DurationRead and does not authorize cross-leaf scheduling or
Core acceptance.

The independent Provider Duration boundary can now consume that checkpoint
directly. It re-reads the checkpoint's exact CourseResource/Unit/Group route,
requires the checkpoint's Course-batch and Browser-plan digests plus current
Micro membership to remain unchanged, and accepts only a study record observed
at or after the completed residence exchange. The resulting Provider-local
readback owner retains the exact residence-result digest, Task, completion
time, observed duration and richer study-record facts. It deliberately does
not infer a required duration delta, remote completion or authority for the
next browser leaf; those remain Core policy/orchestration decisions.

Pause/resume/restart cannot safely enter the accumulated cursor yet. Their
independent durable command/acknowledgement exchange is recoverable, but Core
must first define how it owns or cancels an already issued active
`ResidenceTarget`, how its sequence advances, which terminal state the old
exchange receives and which owner survives recovery. The Provider will not
invent concurrent exchange or replay semantics around that missing runtime
contract.

Capture evidence may replace or refine this plan at any time; neither path is
deferred.

DurationRead never creates an Execution, reports time or mutates remote state.
The independently implemented submission route does not report duration. HTTP
success or 100% completion cannot satisfy any future duration-report acceptance
gate.

The rendered start route is constructed independently from session
credentials. Frozen donor behavior and real upstream issue evidence establish
the minimal current route as:

```text
https://ucontent.unipus.cn/_explorationpc_default/pc.html?cid={courseResourceId}
```

The Provider derives `cid` only after fresh Group detail rebinds its normalized
Course-resource identity and rejects a disagreement between the Course, Group
remote ID and normalized route. The URL validator requires HTTPS, the exact
host/path, one `cid` query pair, no userinfo and no fragment. Cookie, JWT,
openid and Capture material never enter the URL. Optional observed
`theme`/`aitutorialId`/`cloudCurriculaId`/`source` parameters and the
courseware hash are not guessed: the bounded DOM protocol selects the exact
fresh Unit/Section/Micro/Task after the minimal Course page renders.

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

Decrypted byte buffers are zeroizing owners on every success and error path.
Parsed decrypted content, standard-answer and user-module JSON trees likewise
recursively zeroize all owned string values when normalization ends, including
ignored remote fields. Only the explicitly normalized Question or answer facts
cross the parser boundary; debug formatting for the raw JSON owner is redacted.
Executable answer-bearing and preset request JSON is also held by a zeroizing
owner across encoding, header construction, transport failure and cancellation.

Asterism now implements QuestionInventory/QuestionParse, AnswerResolve,
SubmissionBuild, SubmissionExecute and SubmissionVerify as independent
capabilities. Content and standard-answer reads each refresh and bind the exact
CourseResource/Group route, bound/decrypt their own `unipus.` document and drop
ciphertext plus key material after normalization. The immutable draft preview
contains question structure and answer shape but no selected answer values and
no executable provider payload. Each sanitized Question also carries the exact
stable `group:{courseResourceId}:{unitId}:{groupId}` Task identity; answer
resolution and draft construction reject a snapshot from any other route.
The current Rust donor accepts each decrypted module's nested `content` as
either encoded JSON text or an inline object. Both forms enter the same
bounded, recursively zeroizing parser. `direction.text`/`direction.pcText`,
module material and child text are normalized from rich text, while a
multi-child choice keeps each component's own normalized stem and option set.
The same donor reads `contents[].path`, subtitle-track paths and embedded
WEBVTT before external transcription. Asterism now classifies bounded audio,
video and subtitle routes, accepts only HTTPS names rather than literal-IP
hosts, strips fragments, deduplicates in order and retains exact URLs only in a
zeroizing Provider-private media owner. That owner repeats the exact remote
Task and Question identities, and the opaque attachment digest hashes both
with the canonical URL, so the same CDN URL cannot authorize cross-Task reuse.
The persisted Question receives only that attachment ID, kind, bounded label
and subtitle flag, never a fetch URL. The Provider atomically consumes the
complete cached reference set and creates one immutable Question set. If any
Question has media, every ordered Question enters a bounded zeroizing manifest
containing Task/remote identity, position and content fingerprint, while only
media entries carry ordered attachment IDs and canonical routes. Core encrypts
that single continuation at rest alongside the snapshot; entirely media-free
sets return no continuation. Session-aware AnswerResolve requires the exact UAI
type, phase, nonzero revision and digest, then revalidates the complete manifest,
every identity, URL normalization, kind and recomputed attachment ID before any
source can be exposed or any answer network read can begin. It subsequently
refreshes Task detail and encrypted content, so a valid old artifact cannot
bypass protocol drift. The legacy no-artifact standard-answer path remains
unchanged. The same complete manifest is rebound again when an artifact-bearing
SubmissionExecute session is claimed. Only after that validation and a fresh
Task/detail plan does Native HTTP refresh Course/account/progress, freeze its
authenticated session, generated annotator token and exact zeroizing POST
owner, and return the prepared `uai.answer-submit.v1` operation. Core records
the request digest before dispatch. The operation sends once and returns a
separate exact response-body digest plus the accepted receipt; an ambiguous
issued mutation has no Provider replay path. Its recovery hook first validates
the exact continuation revision, operation type, timestamps and nonzero
recorded digest, then repeats only fresh Task/route/account/progress discovery
and request materialization. A changed request fails closed; an identical
request still returns no outcome and keeps the session locked because the
receipt-authorized user-module version cannot be reconstructed safely.
Media-free snapshots retain the
legacy single-call execution path. Embedded WEBVTT is parsed with bounded
input/output, strips its header, cue ordinals and timestamp rows, normalizes
only cue text into transcript metadata and is excluded from the display stem. The
current donor sends its default authorization/cookie header set through its
generic media GET even when the URL host changes; Asterism does not copy that
credential leak. After complete-manifest rebinding, UAI can freeze one exact
immutable GET plan. Its request digest covers the Core Task, remote Task and
Question, position, Question content fingerprint, attachment ID, canonical
URL, kind, subtitle flag, credential scope and byte ceiling. Only exact
the standard `https://ucontent.unipus.cn` origin receives the already scoped
UAI session; every external host or custom port is anonymous. Subtitle, audio and video plans carry separate bounded response
ceilings, permit no redirect and accept only the original canonical route as
the reported final URL. After shared execution, the Provider moves the
downloaded `SecretValue` into a zeroizing response owner only when status is
exactly 200, the executor reports zero redirects, the reported final URL still
equals that canonical route and the actual body is nonempty and within the plan ceiling. Its response digest hashes
the immutable request digest, exact body digest and byte length, preventing a
valid download for another attachment from being substituted downstream.
Response MIME or file-signature inference is intentionally absent because the
donors provide no stable contract for it. The shared downloader must still
enforce DNS/private-range policy and bounded streaming, then bind the accepted
bytes plus model credential to the exact Task and AnswerResolve attempt.

The current Rust donor selects downloaded `.vtt`/`.srt` before its
ffmpeg/Whisper branch and applies the same line semantics already evidenced for
embedded WEBVTT: trim lines, drop blank/header/numeric-ordinal/timestamp rows
and join the remaining cues. Asterism permits that path only when the immutable
manifest marked the accepted response as a subtitle `File`. It requires valid
UTF-8, the existing bounded source/output limits and nonempty normalized text,
then retains the response owner beside a zeroizing transcript and hashes the
response digest with the exact transcript bytes. A non-subtitle response cannot
select this parser. Audio/video bytes still require the shared transcriber and
model/configuration provenance contract.

When a Question has multiple media sources, the same donor traverses all of
them in captured order before joining nonempty transcription results. From the
already rebound complete manifest, UAI can therefore materialize one bounded
nonempty batch containing every exact fetch plan in that order. A batch digest
hashes its count and ordered request digests; duplicate request authorities,
foreign/media-free Questions and any later reordering fail closed. The batch
does not decide whether one failed source is skippable, perform retries, join
transcripts or truncate a model prompt. Those are durable shared
AnswerResolve/transcriber policies and must preserve this frozen order.
When every source succeeds, the batch can consume one response owner per
request only in that same order. A separate set digest hashes the batch digest,
response count and every ordered response digest; missing, extra, duplicate,
reordered or foreign responses fail closed. This complete-success owner does
not imply that a partial donor-style best-effort result is acceptable; that
decision still needs an explicit shared policy and durable per-slot outcomes.

After a durable media submission yields its receipt, session-aware
SubmissionVerify rebinds that same complete manifest to every immutable Draft
Question before starting fresh Task or user-module reads. The existing
receipt-versioned verifier then remains the only authority for answer
persistence and score facts. A foreign type, phase, revision, digest, Task or
Question fails before verification transport; an artifact-free execution uses
the unchanged legacy verifier.

Execution is advertised only when a fresh Group has a bounded positive
`question_num` and either one homogeneous type or exactly one positional type
per Question. The currently lossless mappings cover `single-choice`,
`multichoice`, `short_answer`, `writing`, `translation`, `revise-mistake`,
`material-banked-cloze`, `basic-scoop-content-dropdown`,
`fillblank-scoop-dropdown`, `sequence` and `basic-scoop-content`. They preserve
Text, FillBlank, Ordering and Matching semantics instead of flattening every
shape into a generic choice/text answer. A choice module with multiple native
children becomes one Composite Question whose per-child choice kind and option
set remain in sanitized metadata; its Provider-native Composite answer and
execution plan preserve every child instead of flattening duplicate A/B option
labels across parts. Immutable draft items remain in exact positions `1..n`.
`video-popup` is a donor-audited media container rather than a fixed answer
kind. The MIT donor reads its standard-answer children, while the current Rust
donor dispatches on the freshly decrypted module `replyType`. Asterism therefore
advertises the capability from the tree label but derives the actual Question
as single choice, multiple choice, multi-child composite choice or ordered
FillBlank text from the bounded module/child reply labels. Missing reply labels
follow the current donor's `text-area` default; unknown labels, empty children
and choice/text mixtures fail closed as protocol drift.
The MIT donor's `writing` path reads the same bounded standard-answer analysis
shape and emits one textual answer, so it maps losslessly to the shared
ShortAnswer semantic instead of being dropped as an unknown type. The donor's
short-answer fallback accepts a bounded plain
`analysis` string, a top-level JSON `analysis` string or ordered child analysis
rows when the standard `answer` document is absent; all three normalize to
Text answers before leaving the zeroizing decrypted owner. Across choice,
text, ordering and matching rows, donor `answers` takes precedence only when
non-empty; an empty `answers` array falls back to the sibling `value` array as
the audited Python implementation does.
The execution boundary additionally requires the explicit unique positive
numeric answer-entry instance ID used by the donors for every module; arbitrary
strings remain read-only and cannot reach the POST. The native body is
constructed only inside the execution boundary and preserves module order,
per-module answer children and globally flattened completion/judge order. Every
body requires the bounded child/module `type` and child `replyType` facts
retained by the answer-free parser, using the current donor's exact per-child
judge labels instead of guessing them from the Group `base`. `quesDatas`
retains the independently observed cardinality-specific client versions:
single-module compatibility uses `contextVersion=0`/`answerVersion=0`, while
multi-module uses the current Rust donor's coherent minimal `1/1`. This is
separate from `thirdPartyJudges[].versions`: the current donor reads the fresh
Course `publish_version` and emits it as `course` with `answer=3`; the MIT
compatibility builder independently evidences `course=0`/`answer=0`. Asterism
normalizes a bounded positive numeric or numeric-string publish version into
every fresh Group Task and uses the current pair when that fact is present.
Native inventory takes the required current value from Course progress,
cross-checks tree and per-Unit copies, and fills a missing tree field. Only an
explicitly injected legacy transport that omits Course progress and every
per-Unit version can select the compatibility pair; Asterism never fabricates
a Course version or adds MIT's client-authored score maps to ordinary answer
submissions. Apache's separately evidenced empty-placeholder ResourceExecution
retains its own bounded map in that dedicated wire mode. A code-`0`,
version-bearing response becomes an accepted receipt; `600001` and `600002`
become typed retry state without a receipt, and the mutation is not implicitly
repeated. A Network failure from the mutation boundary is likewise returned
after one attempt so Core can retain verify-only recovery authority without
replaying the POST.

The MIT donor classifies `video-popup` as answer-bearing but its submit builder
places that exact base in the study-mode set; the Apache donor independently
uses the same rule. Consequently, Asterism preserves its complete `quesDatas`,
completion and judge rows while emitting `submitType=2` when every Question in
the plan is `video-popup`. Any mixed ordinary/video plan uses `submitType=1`.
This answer-bearing type-2 path is distinct from the empty preset completion
body and remains under ordinary SubmissionExecute/SubmissionVerify recovery.

Immediately before an answer-bearing POST, the native mutation client refreshes
the Course detail and exact Unit progress through the same account session. It
requires the exact Group to be an incomplete `tab_type=task` leaf and requires
the fresh donor availability window to contain the current time. A completed,
non-task, unavailable, missing or identity-mismatched leaf fails before body
construction and transport. Only then does it materialize a redacted zeroizing
request owner. The owner's digest binds the exact POST URL, content type and
complete body, including fresh Course instance, account openid and all immutable
answers; Native HTTP sends those same owned bytes rather than rebuilding the
request. This pre-dispatch identity remains distinct from the Draft and semantic
plan. The mutation client then sends the account-bound JWT and annotator token.
Capture recipe v4 additionally preserves the donor-proven browser Cookie and
optional `u-school` header. The native transport injects those optional values
only into `ucontent` requests, along with the exact account-bound
`u-openid`/`x-csrftoken`, app/platform and Origin/Referer headers; it never
forwards Cookie or school material to `uai` or `ucloud`, and never
misclassifies the school identifier as an access token.
Captured fields remain purpose-bound, allowlisted and zeroized; the Provider
stays Development until both native and fallback paths receive live validation.

Verification requires that accepted receipt and reads only the exact
`{groupId}-{submitVersion}` user-module route after refreshing the Course
instance. The response must bind both the top-level and submit-info Course to
that fresh route, repeat the exact `{groupId}-{submitVersion}` module identity,
bind Group and both version fields, contain the complete exact ordered native
Question/module set, keep every row in submitted state, mark every answer child
done, and reproduce every immutable draft answer exactly. Only then are all
Questions Confirmed; a missing, extra, duplicate, reordered or changed row
fails the whole verification. The same readback may expose a score only when
its bounded `__SUMMARY__.answerList` contains the MIT donor's counted numeric
`questionType=1|3`; in that case `__SUBMIT_INFO__.state.score_avg` must be a
valid decimal `0..=100` and is converted to fixed-point thousandths. A genuine
zero remains an explicit score for Core policy/alerting, while an absent or
entirely uncounted summary yields no score fact. Progress and Task completion
are never inferred from this value. With no receipt the result is
Inconclusive and no version route is guessed or read.

For the ordinary answer, single-upload, compound-upload and compound-oral
verifiers, the same already-bound document can also produce a Provider-private
policy-evidence owner. Each adapter first proves its exact immutable answer,
object key or ordered answer/oral pair. The owner then hashes the exact bounded
result bytes and retains the Group/version binding, positive `strategyId`, exact boolean `required`,
`record_every_submit` and `record_max_submit`, decimal `task_mini_score_pct` in
fixed-point thousandths, second-resolution `startTime`/`endTime`, and
submit-state `expired`, `not_start` and `lastSubmit`.
Zero time endpoints mean no bounded window and zero `lastSubmit` means absent,
matching the other UAI strategy parsers. A document with neither `strategyId`
nor `strategy` keeps the legacy verification path; once either appears, the
complete typed policy is mandatory and malformed, partial, reversed-window or
out-of-range material fails closed. The result owner is evidence only: the
shared verification projection remains unchanged, recording flags grant no
retake authority, and no score-policy precedence is inferred. The four owners
remain adapter-specific; a shared evidence contract is still required to
consume the policy without conflating ordinary, upload and compound shapes.

Unknown or structurally ambiguous types still fail closed, but this is a drift
guard rather than a policy exclusion. Discussion, exit-ticket, oral-empty and
artifact-upload mutations each retain their donor-specific execution and
verification semantics and continue through shared contract work. This remains
synthetic offline coverage and is not a live compatibility claim.

## Answer evidence, bootstrap and post-completion policy

The four-donor full sweep found no account-wide UAI answer-history or attempt-
version enumeration route. The executable MIT read is exactly
`user_module/{courseInstance}/{groupId}-{acceptedSubmitVersion}` and is called
only after the corresponding mutation returns that version. The current Rust
donor contains an unused `fetch_my_records(groupId)` module marked as
pre-reserved; its wall-clock suffix and `data.rows` parsing have no caller,
fixture, pagination or documented version semantics. It is insufficient
authority for a first-bind harvest. UAI bootstrap may inventory existing
Courses/Tasks/progress read-only, but answer harvesting must report partial/
unsupported rather than guess a version or create a submission.

The exact receipt-bound response does expose reusable evidence. `quesData`
retains each native module, submitted context and every completed submitted
child value. `__SUMMARY__.answerList` additionally exposes bounded `done`,
`right`, `questionType`, `student_answer` and protocol-version facts. Those
summary rows can describe module children, whereas the current shared Draft
represents a UAI multi-child module as one Composite Question. The Provider
must not align summary rows to Draft items by map order or promote a task-level
score to correctness. A shared child-evidence/provenance result contract must
land before UAI can append `right` as `VerifiedHistorical`; exact submitted
values and the optional fixed-point score remain suitable incremental facts.

Strict Completion and Score Improvement are separate default-enabled Core state
machines. For UAI, strict completion is only a fresh exact Group observation
with `pass=1`, `pass2=1` and `perm=1`. Course/Unit `finishProgress=100`, a submit
receipt, verified answer persistence, duration or score cannot replace it. An
incomplete Task may receive another ordinary attempt only after fresh
`tab_type=task`, exact identity and availability-window preflight.

Scoring facts also remain separate: progress supplies Group
`min_score_pct`; user-module strategy may supply `task_mini_score_pct`; Course
policy supplies Unit `passScore`/`scoreType` plus Course `scoringMode`; study
records expose `scoreTaskFlag` and optional total score. No donor establishes a
universal precedence rule between them. The receipt-bound Provider owner now
preserves the user-module fact and its exact-result provenance, but shared
typed reconciliation remains a Core/Domain gap.

Every executable donor skips an already completed Group. No donor or issue
provides a reset/redo request, retake-attempt creation route, remaining-attempt
count or positive eligibility response. Rust issue 4 is only a request to
repeat a Course displayed as completed. `record_every_submit` and
`record_max_submit` are observed recording strategies, not retake authority.
Score Improvement must therefore leave UAI retake unavailable/unknown and must
not invoke ordinary SubmissionExecute on a completed Group. Owner-global corpus
storage, first-bind harvest recovery, incremental evidence ingestion, typed
scoring reconciliation and both policy machines remain Main-owned Core gaps.

The MIT and current Rust donors complete pure-study Groups with
`quesDatas=[]`, `isCompleted=[]`, `thirdPartyJudges="[]"` and `submitType=2`,
followed by fresh progress reads. The current donor chooses that body only for
fresh progress leaves whose `tab_type` is exactly `text` or `video`. The older
donors agree that `rich-text-read`, `text-learn`, `vocabulary`, `input` and
`video-point-read` are the five preset `base` labels. Independently, the Apache
donor's active generic preset path inserts `instanceId=0` placeholder Question
rows, pads a short base list by repeating its final type, truncates an overlong
list, and emits one ordered judge per declared Question. Asterism retains both
wire behaviors under a frozen runtime choice rather than treating one as
evidence against the other. The operation has no
Question/SelectedAnswer pair and remains separate from the submission-draft
protocol. Its execution acknowledgement remains unverified. The independent
goal-bound verification method re-runs fresh Task detail binding and the exact
per-Unit Group progress read; only `pass=pass2=perm=1` returns a verified
Completed outcome. Unknown progress remains unverified so Core can retry the
read-only verifier without replaying the mutation.

Completion diagnosis is narrower than progress parsing. The same fresh Task
detail preserves the exact Group `closes_at`, and the subsequent exact progress
read supplies its observation time. If the Group is still incomplete and its
positive close time is at or before that observation, the sanitized verified
execution result records `window_closed=true`; only that exact versioned result
maps to shared `WindowClosed`. A future start time is not mislabeled as closed.
No donor assigns stable reason semantics to an individual zero among `pass`,
`pass2` and `perm`, or proves that Group/Course score, duration, `required`,
`statistic_mode_out`, prerequisite or review facts caused this Group to remain
incomplete, so those cases deliberately retain `RemoteUnknown`.

SubmissionVerify has no corresponding safe override. Its shared snapshot
contains answer persistence and an optional overall score but cannot carry the
Provider-private user-module strategy/window owner. Neither fact explains the
three completion flags, and smuggling a diagnosis through score, percentage or
status would destroy their existing semantics. Shared typed policy evidence is
required before receipt `expired`/`not_start` can participate directly.

Asterism registers ResourceExecution for a non-empty set containing those five
audited labels and, independently, for exact `exit-ticket`, exact single
`discussion`, exact `short_answer`, `oral-sentence`, `video-dub` and
`oral-personal-state`; every such Task is marked with ExecutionVerify plus
ProgressRead. Exact `short_answer` also retains QuestionInventory/QuestionParse,
AnswerResolve, SubmissionBuild, SubmissionExecute and SubmissionVerify: choosing
the Apache donor's `SUBJECTIVE_ALLOW_EMPTY` ResourceExecution is a separate
authorization, not an implicit fallback from normal answer submission. Before mutation
the native transport refreshes Course detail and the exact Unit progress
document, requires the exact Group leaf and `tab_type=text|video` for a preset
or `tab_type=task` for exit-ticket/discussion/subjective/oral, requires the fresh donor
availability window to contain the current time, and skips the POST if all
three completion flags are already `1`. Preset freezes the resolved runtime
mode: `marker` sends the MIT/current-donor no-Question `submitType=2` body,
while `placeholder` uses Apache's expanded ordered Task types, repeated
`instanceId=0` empty children and score/judge map in `submitType=1`. Exit-ticket
uses its exact no-Question marker. Direct discussion independently freezes the
same choices: `marker` sends the no-Question body, while `placeholder`
uses the donor's `instanceId=0`, empty child, courseAnswer map and discussion
judge in a `submitType=1` body. Oral sends its distinct bounded
placeholder-answer body with
`instanceId=0`, one empty child per declared Question, `submitType=1` and the
audited score/judge structure. The Course judge version is taken only from the
fresh TaskDetail/Course-progress snapshot; missing, zero or unsigned values
outside the signed protocol range fail before transport, and the captured
donor example value is never hardcoded. After this fresh preflight, every
marker or placeholder body is moved into the same redacted zeroizing request
owner used by ordinary answer submission. Its digest binds the exact common
POST route, content type and full body; Native HTTP sends only those owned
bytes. The shared TaskExecution contract still needs a prepare-before-send
digest registration hook. Each body is sent once, validates an accepted
version acknowledgement, and returns an explicitly unverified outcome. Core
persists the mutation attempt before the call, accepts success only from the
existing exact Group progress reader, and uses that reader alone after
ambiguous transport failure, worker interruption or pending readback. The POST
is never replayed and neither its receipt nor a duration observation is
completion evidence. Only the exact single-discussion direct-empty behaviors
join this path; the reply/readback workflow remains a separately authorized
two-mutation family. Upload and mixed/compound modes do not inherit it;
exit-ticket and each oral family are likewise separately classified, and the
other audited known families remain required dedicated capabilities rather
than exclusions.

The Apache donor's `SUBJECTIVE_TYPES={"short_answer"}` branch and
`SUBJECTIVE_ALLOW_EMPTY` setting call the same generic simple builder with an
empty answer list. Asterism therefore maps only an exact homogeneous
`short_answer` Group with a bounded positive Question count to a dedicated
subjective-empty ResourceExecution. The native body repeats one
`instanceId=0` empty child and `short-answer` judge per declared Question, uses
the fresh positive Course publish version, and runs only after the same exact
incomplete available `tab_type=task` preflight. The accepted submit version is
an acknowledgement, while fresh exact Group progress remains the sole
ExecutionVerify authority. This path neither reads nor overwrites an immutable
normal Submission Draft and cannot be selected merely because answer resolution
failed.

## Additional donor capability families

The Apache donor provides reliable behavior evidence beyond ordinary question
submission:

- discussion uses `ucloud.unipus.cn/api/bbs/utopic/page`, paginated
  `ureply/top/page`, `ureply/add`, then the normal Group completion mutation;
- exit-ticket and configured oral flows use their own empty-submission shapes;
- ordered `basic-scoop-content,oral-sentence` uses one compound body whose two
  native instances come from the same encrypted standard-answer array;
- `multiFileUpload` first requests a CMS upload token/file key, uploads the
  bounded artifact to the returned object-store route, then submits that key in
  the immutable answer flow;
- subjective/discussion answers may come from an external OpenAI-compatible
  model, while the current Rust donor can transcribe referenced audio/video
  before resolving an answer.

All are active implementation scope. The Provider also exposes both of the
donor's independent direct-empty ResourceExecution shapes for one exact single
`discussion` Task. Master resolves `marker` or `placeholder` at Provider,
account or Task scope and Core freezes the selection in the attempt snapshot.
Marker uses one `submitType=2` no-Question body; placeholder reproduces the
donor's `instanceId=0` empty child and score/judge map with the current fresh
Course publish version instead of its captured constant. Both use the same
fresh Task/progress gate and progress-only verification, but neither falsely
claims that a topic reply exists. The richer
Provider-private discussion protocol now derives Course instance, class,
curricula and current-user facts from fresh
Course/detail/user-info reads; builds the exact topic/reply-page/reply-add
bodies; bounds every page and response; binds each reply snapshot to the
requested topic; zeroizes reply content; treats the add response as a receipt
only; and can verify exact topic, current-user and content through paginated
readback. Before reply dispatch, its stable request digest binds the complete
fresh Course/class/curricula/current-user/Group route, topic and exact content
without persisting them in audit metadata. That exact readback plus a fresh exact Task containing only one
`discussion` module freezes a second, content-free completion plan with a hash
of the verified reply. A second request digest independently binds that reply
hash to the fresh Task fingerprint, hierarchy and topic. Native HTTP then refreshes Course instance and the exact
incomplete available `tab_type=task` progress leaf before sending the donor's
empty `submitType=2` Group mutation once. Its accepted version is a separate
receipt and only fresh exact Group progress can confirm completion.
UAI now projects this topology through a content- and credential-free
`uai.discussion.reply-plan.v1` scheduling artifact. Its
`uai.discussion.reply-complete.v1` sequence has exactly two independent,
single-occurrence `AcceptedMaximumReached` phases: reply submit, then Group
completion. The second phase requires the namespaced
`uai.discussion.reply-readback.v1` observation. UAI persists the accepted reply
receipt first, then records its independently verified readback against ordinal
one, then records the phase-two observation. The observation digest joins the
original reply-request digest, the exact verified reply digest and the dynamic
completion-request digest, so a fresh Task fingerprint or completion-plan
change cannot reuse an older gate. Normal persistence returns a typed readback
gate only after both records succeed. If a crash lands between them, exact
Core recovery evidence authorizes writing only the missing observation; an
already complete observation can restore the same typed gate without rewriting
verification. The gate binds the exact phase-two request before issue.
Reply and completion acknowledgement classifiers create separate response
digests only after their bounded parsers accept the response. Rejection,
malformed input and transport ambiguity create no receipt, leaving that exact
issued ordinal locked.

Core's `ExecutionMutationSequenceRecoverySnapshot` is consumed only as
read-only evidence. UAI recomputes the artifact/plan, both request identities
and the completion-bound observation, rejects Core-valid but donor-impossible
false receipts, and distinguishes no mutation evidence, ambiguous reply,
accepted reply awaiting readback, verification awaiting its gate, recorded
gate, ambiguous completion and accepted completion awaiting progress. None of
those states grants replay, and the sanitized artifact cannot reconstruct the
reply content. Cross-Provider registration therefore still requires a
scheduling-time immutable discussion Draft/capability binding the zeroizing
content to this topology and invoking the Provider adapter; fresh exact Group
progress remains final completion authority. External media-source
orchestration still requires the shared downloader/model contracts; ordinary
QuestionSession persistence, session-aware AnswerResolve and ordinary
answer-bearing prepared SubmissionExecute handoff are now integrated. The
Provider-private upload
boundary starts by canonicalizing the platform's camel-case
`multiFileUpload` label at Task-tree parsing; ordinary UAI labels remain
lowercase, but this label is never stored as an unreachable
`multifileupload`. It then re-reads exact TaskDetail and freezes the stable
hierarchy, Task fingerprint, unique positional upload module and selected
artifact digest into an upload intent. CMS grant acquisition accepts that
intent rather than raw Course/Group strings, then fresh-binds Course
resource/detail and current app user before requesting the exact token route.
The grant remains tied to the same intent/artifact and a swapped artifact fails
before multipart emission. Token and artifact bytes stay in zeroizing owners,
audio/mpeg artifacts are bounded to 64 MiB, the multipart request has fixed
fields for the audited Qiniu origin, and the object-store response must repeat
the granted file key. Its typed accepted-result owner preserves the exact
response digest and an optional bounded nonempty `hash`/Qiniu ETag when present;
key-only customized responses remain compatible, while a malformed present
hash fails closed. Because that request is fully determined before dispatch,
the Provider hashes its fixed POST origin, deterministic content type and full
zeroizing multipart body (including token, key and artifact bytes) into a
stable request digest. A changed token or any body byte changes the digest. CMS
grant now materializes one redacted immutable request only after fresh
Course-instance and app-user discovery; its digest binds the exact GET query
URL and required Referer. Final Group submit likewise materializes one
zeroizing request after fresh Course/progress/account rebinding; its distinct
digest binds exact POST origin, content type and complete body, including the
fresh Course instance and openid. Native HTTP sends the URL, headers and body
from those same request owners, while semantic intent fingerprints remain
separate. A successful response becomes a strong uploaded-artifact
owner retaining remote Task, Task fingerprint, Course/Unit/Group, positional
upload module, exact key, artifact digest, intent fingerprint, exact multipart
request digest, response digest and optional hash; its route/hash data is
redacted in debug and zeroized on drop. The same complete successor can encode
bounded zeroizing `uai.upload.object.v1` bytes. Decode requires the independently
persisted issue request and accepted receipt response digests and reconstructs
the complete uploaded-artifact binding without retaining the grant token. This
is still mutation output, not independent object readback. The Provider also
preserves the donor's 4 KiB minimal-MP3 artifact as an explicit option.

For a Group containing only one `multiFileUpload`, the Provider re-reads exact
Task detail after object upload and requires the same Task fingerprint,
Course/Unit/Group identities, upload position 1 and current positive Course
publish version. The immutable Provider-private final plan uses the donor's
`instanceId=0` with one completed child whose `value` is the exact uploaded
key. The outer minimal question/judge body uses the fresh publish version and
is built only inside the native mutation boundary. The resulting redacted
request exposes only its digest for durable pre-dispatch registration and
zeroizes the full body on drop. Immediately before the
single POST, native HTTP refreshes Course instance and the exact incomplete,
available `tab_type=task` Unit-progress leaf. A version-bearing response is a
receipt only. The key-bearing body and route identities stay in zeroizing
owners, and an ambiguous mutation is not replayed.

The accepted final receipt supplies the only permitted user-module version.
Native verification refreshes and binds the Course route, then requires exact
`{groupId}-{version}` module and submit-info state. The upload-specific parser
accepts exactly one numeric/string-zero `instanceId`, submitted context and one
completed child whose sole value equals the immutable uploaded key. Any missing
receipt, changed key, extra module/child/value or unsubmitted state fails
closed. After those checks, the verification owner retains the exact bounded
user-module document digest independently of optional policy fields. This
provides the future mutation-verification observation only when recovery also
has the exact final plan and accepted submission version; the digest alone
cannot restore either. It confirms answer persistence only and still requires
a separate fresh exact Group progress read for completion.

The donor's mixed `multichoice,multiFileUpload` flow is one atomic Group
submission. Asterism therefore still refuses to submit only its upload half.
The Provider-private compound preparation accepts exactly one already validated
ordinary `multichoice` sub-draft plus one Task-bound uploaded artifact, then
re-reads the full Task and requires exact ordered
`multichoice,multiFileUpload`, upload position 2, the unchanged Task
fingerprint/hierarchy and a current publish version. After the same fresh
incomplete/available task-leaf and account rebind, Native HTTP materializes one
zeroizing request owner whose digest binds the exact POST URL, content type and
complete choice-plus-key body. The semantic plan fingerprint remains separate,
and the sender consumes the same owner rather than rebuilding its wire bytes.
Receipt-versioned readback must contain exactly the immutable choice
answer followed by module `0` with the exact object key; reversed, extra or
changed modules fail closed. Its verification owner likewise retains the exact
readback digest independently of optional policy fields. The shared artifact
lifecycle, durable attempt and one compound Draft that owns both slots remain
active Core integration work. A grant, object-store response, accepted final
receipt or even exact two-module readback alone is never Group completion
evidence.

Core's receipt-conditional atomic sequence now covers the bounded stage-advance
rules for staged upload. Each mutating phase can advance only from its definite
parsed receipt, while its exact request digest is persisted at issue time. The
Qiniu POST response repeating the exact key remains a mutation receipt; no
audited donor route performs a separate object-store readback, and Asterism does
not relabel that response as verification. UAI can now reconstruct the exact
Provider output from encrypted `uai.upload.object.v1`, but Core still has no
atomic receipt-plus-state repository: its ordinary receipt retains only a
response digest and acceptance bit, while final UAI submission requires the
exact object key produced by the grant and confirmed by Qiniu. Core's sanitized
scheduling artifact also cannot own or recover the bounded audio bytes, and an
artifact digest alone is not a recoverable owner/account/Task-bound handle. The
upload sequence must therefore remain Provider-private until the shared
Artifact/Draft and encrypted stage-output capability atomically binds those
bytes, the returned object state and, for mixed Groups, the ordinary sub-Draft
into one durable Attempt.

Once an exact single or compound final plan already exists, UAI now projects a
credential-free `uai.upload.final-plan.v1` scheduling artifact whose binding
digest covers the complete semantic plan without exposing the object key or
ordinary selected answer. For a compound plan, the digest directly covers the
ordinary protocol versions, ordered Question identity/type, every ordered
answer child/value and every judge question/reply type in addition to the
immutable Draft ID and upload lineage. A same-ID selected-answer substitution
therefore changes the scheduling artifact and sequence-plan digests before any
issue. Separately, the bounded zeroizing
`uai.upload.final-plan-state.v1` codec retains the complete private plan. The
single form owns the exact object key, Task hierarchy, artifact/intent lineage,
Course version, semantic fingerprint and fully materialized final request
digest. The compound form additionally owns
the ordinary Draft ID, Task fingerprint, selected child values, judge labels
and current protocol versions. Decode recomputes the compact artifact and
sequence plan and requires both independent digests to match; changing the key,
answer, judge, hierarchy or otherwise-valid sequence fails before the plan is
exposed. Its one-phase `uai.upload.final-submit.v1` sequence
permits at most two `uai.upload.final-submit` ordinals and uses Core's
`AcceptedOrMaximumReached` condition. Every concrete single/compound request
also carries the same semantic binding digest before its exact issue-time wire
digest is registered. A code-0 version-bearing response produces an accepted
mutation receipt and immediately closes the phase. Only a structurally valid
numeric `600001` or `600002` response becomes an `accepted=false` mutation
receipt through `new_retryable_rejection(..., 120)`. Core atomically derives
and persists `retry_not_before` with that receipt, rejects ordinal two both
immediately and at 119 seconds, and permits issuance at the exact 120-second
boundary.
Other rejection codes, malformed responses and transport ambiguity return an
error without recording a receipt, so the issued ordinal remains locked. Each
classified outcome also binds its exact ordinal; even an identical second wire
request cannot reuse the first ordinal's result. The existing Native HTTP
single/compound paths use this same classifier but retain their legacy typed
`RateLimited` result until the shared stage-state executor can own the whole
workflow.

A definite accepted outcome can now materialize a bounded zeroizing
`uai.upload.final-result.v1` owner and deterministic secret bytes. It retains
the accepted submission version independently of Core's hash-only receipt and
binds the single/compound kind, ordinal, sequence plan/artifact digests,
semantic plan binding, exact request digest and response digest. Decoding
requires an accepted `uai.upload.final-submit` recovery record with the same
ordinal and request/response lineage; rejected results, unknown fields, digest
substitution or a foreign otherwise-valid sequence fail closed. This owner is
only the Provider input for the future atomic receipt-plus-encrypted-state
transaction. It now also preserves the exact accepted-receipt timestamp. Once
decoded against the accepted Core record, UAI must additionally decode the
complete single or compound final-plan state against the independently stored
compact sequence. Only that exact plan may reconstruct the
bounded accepted receipt, parse the receipt-versioned user-module, and produce
an `ExecutionMutationVerification` whose observation digest is the exact
readback digest. For compound upload this rebind includes the ordinary selected
children and judge descriptors as well as the object key, so a same-Draft-ID
answer substitution cannot inherit an accepted result. The state is not stored
by the current sequence sink, contains no object key or compound selected
answer and therefore cannot independently authorize readback, replay or final
mutation verification.

Core's encrypted parent-authority/complete-batch snapshot is not a substitute
for this stage state. Final upload is one receipt-conditional mutation stage,
not a parent/child batch, and the parent snapshot contract does not atomically
commit an accepted mutation receipt with `uai.upload.final-result.v1` or retain
and type-bind `uai.upload.final-plan-state.v1`. UAI therefore keeps the new read-only
verification adapter Provider-private until the dedicated stage-output
transaction exists.

### Minimal durable upload stage state

The smallest shared contract that closes this gap has one immutable encrypted
input plus Provider-owned stage states. Every record must be bound to the same
`owner_user_id`, `provider_account_id`, Core `task_id`, `execution_id`,
`execution_attempt_id`, `provider_id=uai`, Provider-plan artifact digest and
sequence-plan digest. A stage record additionally binds its namespaced
artifact type/version/digest, phase position, source or target mutation ordinal,
operation type, predecessor-state digest, exact input-artifact digest and the
matching issue request/receipt response digests where that stage has them.
Core validates the relational owner/account/Task/Attempt chain before
decrypting; UAI then validates the
remote Task/fingerprint, Course/Unit/Group, upload position, intent fingerprint
and artifact digest inside the zeroizing payload. Neither layer may authorize a
stage from hashes alone.

| State | Earliest durable point | Encrypted Provider payload | Successor authority |
|---|---|---|---|
| `uai.upload.input.v1` | Before sequence preparation | bounded filename, `audio/mpeg` and exact bytes, plus the immutable single-upload or compound-Draft binding | UAI reconstructs `UaiUploadArtifact`, recomputes its digest and freshly prepares the upload intent |
| `uai.upload.grant.v1` | After an exact code-200 CMS document parses, before any Qiniu issue | zeroizing token and exact file key plus the complete `UaiUploadGrant` Task/artifact/intent binding and grant request/response digests | Only the exact input artifact may materialize the Qiniu request; a persisted grant is reused, never silently replaced |
| `uai.upload.object.v1` | In the same transaction as the accepted Qiniu mutation receipt | exact returned key, optional bounded returned `hash`/Qiniu ETag, response digest and the complete `UaiUploadedArtifact` binding; the token is no longer copied forward | Only this accepted state, not the receipt hash alone, may prepare single or compound final submission |
| `uai.upload.final-plan-state.v1` | After fresh Task/progress/account rebinding and final request materialization, before final-submit issue, atomically with that issue or already durable | exact single/compound semantic plan, Course publish version, uploaded key/artifact lineage, exact final request digest, immutable Draft reference and complete selected child/judge material for compound form; it must reproduce compact `uai.upload.final-plan.v1` and the sequence digest | Recovery can verify the issued request identity but can never replay an issued request without a receipt |
| `uai.upload.final-result.v1` | In the same transaction as a definite accepted final-submit receipt | accepted submission version, final request/response digests and final-plan digest | Receipt-versioned user-module readback may retry read-only; fresh TaskProgressRead remains a separate completion authority |

The Qiniu primary documentation describes a normal 200 response containing
both `hash` and `key`, while the audited donor consumes only `key` and an upload
token may customize the response body. The key is therefore required and must
equal the CMS grant. A present hash should be preserved only when it is a
bounded safe string; absence stays compatible, and the hash is neither final
submission input nor an independent object readback.

Receipt plus stage-output persistence is one atomic transition. An accepted
Qiniu receipt without `uai.upload.object.v1`, a final receipt without the exact
submission version, or a stage output without its matching accepted receipt is
an invalid durable state. Exact duplicate writes are idempotent; any changed
digest, ordinal, predecessor, owner, account, Task, artifact or Attempt is a
conflict. Earlier secret material may be crypto-erased only after its successor
state is durable: in particular, the grant token can be destroyed after the
Qiniu receipt and object state commit together, while immutable digest lineage
remains.

The recovery/retry boundary is deliberately narrower than the donor's local
exception loop:

| Point of failure | Automatic action permitted |
|---|---|
| Input validation, fresh reads or request construction before issue | Retry; no remote mutation authority was consumed |
| CMS grant request before any Qiniu issue | Re-read/reissue only when no grant state is durable; it is prerequisite credential acquisition, not Task completion. Once persisted, reuse that exact state or fail the Attempt rather than overwrite it; no token TTL is guessed |
| Qiniu request after durable issue but before an atomic receipt/output commit | Never replay. The result is ambiguous and no audited object-stat authority is available. A new grant is not assumed to produce a different key |
| Definite rejected Qiniu response | Record the rejection; no successor output is created. A new Attempt may start only from an explicitly reusable input artifact and a fresh grant |
| Final submit after durable issue but before an atomic receipt/result commit | Never replay. Fresh progress may diagnose completion but cannot recover the receipt version or prove exact answer persistence |
| Definite final code `600001`/`600002` | Record a rejected receipt and typed retry time before any later issue. The donor permits one delayed retry; no sleep or replay occurs inside the Provider transport |
| Accepted final receipt and durable result | Retry only the receipt-versioned readback and fresh progress reads; never repeat either mutation |

Core's `AcceptedOrMaximumReached` condition now expresses "stop on the first
accepted occurrence, otherwise repeat a definite rejection up to the bounded
donor maximum" without forcing a second POST after success. UAI's classifier
further narrows the repeat authority to the two evidenced throttle codes; a
generic `accepted=false` result cannot be constructed from any other response.
Core now persists the 120-second retry deadline and rejects early successor
issuance. Scheduler wake-up/executor integration and the encrypted stage-output
contract above remain shared work.

The donor's `basic-scoop-content,oral-sentence` path is likewise atomic rather
than a normal matching submit followed by an unrelated oral completion. The
Provider-private preparation first rebuilds one immutable matching sub-Draft,
freshly requires the exact ordered two-module Task and current positive Course
publish version, then reads the Group's encrypted standard-answer array under
the same account boundary. Entry one must repeat the Draft's native instance;
entry two supplies an explicit bounded numeric oral instance. A missing/empty
donor answer becomes one scalar-empty child, while bounded parsed
children with no truthy `answers` or `value` material remain empty arrays. The
same donor's single-module builder proves that each child's own
`answersExtra` becomes that submitted child's `extra`; Asterism uses this
stable semantic instead of copying the compound helper's accidental
module/child cross-index. Bounded non-empty text, text-list, number and boolean
values retain their exact JSON value and donor judge string, and dynamic extras
remain zeroizing Provider-owned JSON. Object values fail closed because the
donor relies on Python-specific object stringification rather than a stable
wire encoding.

The native mutation re-resolves Course instance and the exact incomplete,
available `tab_type=task` leaf, then materializes one zeroizing request owner
containing the matching answer followed by the frozen oral slot in one
`submitType=1` POST. Its request digest binds exact POST URL, content type,
fresh Course instance, account openid and complete body independently from the
semantic plan fingerprint; Native HTTP sends those same owned bytes. Every
judge uses the fresh Course publish version and the oral judge retains exact
`oral-sentence` labels. An
accepted version is only a receipt. Verification reads exactly that version
and requires the complete ordered two-module user-module state, exact ordinary
answer equality, the same oral instance, and every exact scalar-empty,
empty-array or non-empty child value plus optional extra. Fresh Group progress
remains the only completion authority. Shared Core still needs one compound
Draft/Attempt slot before the Provider-private preparation and transport can be
exposed as a public capability.

## Sanitized protocol observations

Selected fail-closed parser exits attach only a bounded structural observation
to their `ProtocolDrift` error. Unknown Task-tree roles use
`uai.task-node-role-observation.v1` with the role SHA-256 and byte length, node
field count and child presence. Unknown module or child `video-popup`
`replyType` values use `uai.question-reply-type-observation.v1` with a static
scope, JSON kind and optional string SHA-256/byte length. Invalid or oversized
user-module score summaries use `uai.score-summary-observation.v1` with only
the summary and `answerList` JSON kinds, presence and collection cardinality.

These observations never contain the original response, unknown label,
question/answer content, field-name list, route, account/task identity, Cookie,
token or other credential material. Failure to construct the sanitized shape
does not weaken fail-closed behavior. Unsupported composite-child kinds,
incompatible mixed child shapes, per-row `questionType` drift and generic
missing/changed fields remain unobserved until a similarly minimal schema is
proved safe.
