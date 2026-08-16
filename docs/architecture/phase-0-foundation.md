# Phase 0 foundation

This document records the first executable architecture checkpoint. The local
project plan remains the source of truth for product scope and roadmap.

## Boundaries established

- `asterism-domain` owns persistence-independent entities and invariants.
- `asterism-provider-api` exposes capability-specific traits and rejects
  registry entries whose metadata disagrees with their implementations.
- `asterism-events` carries structured live events. Durable delivery is backed
  by the transactional outbox schema rather than the in-memory bus.
- `asterism-storage` owns the SQLite adapter, formal migrations, WAL/busy
  timeout/foreign-key policy, and repository contracts.
- `asterism-api` owns the versioned HTTP transport and stable error envelope.
- `asterismd` is the only normal SQLite writer and owns process lifetime.
- `asterismctl` is an HTTP client; it does not bypass Core by writing SQLite.

The model intentionally keeps these pairs separate:

| Remote or source truth | Local orchestration truth |
|---|---|
| `Task` | `Execution` and `ExecutionAttempt` |
| `RemoteState` | `OrchestrationState` |
| `AuthMethod` | `SessionKind` |
| `AutomationPlan` | discovered remote `Task` |
| Master-owned Provider runtime settings | user-facing AutomationPlan policy |
| available credit | reserved credit and immutable transactions |

Provider runtime tuning is an administrator control plane, not a bag of fields
on `AutomationPlan`. Master-managed settings resolve from Core-safe defaults to
Provider defaults, optional ProviderAccount overrides and finally a single Task
override. This covers provider-specific execution and discovery parameters such
as bounded concurrency, playback speed and periodic task discovery intervals;
those examples do not limit the extensible setting set to one provider or one
capability.

Providers own a bounded, versioned settings schema and safety validation. Core
owns scope resolution, permissions, revisioning, audit and persistence, and an
Execution retains the resolved immutable settings snapshot. Scheduler remains
the sole owner of recurring discovery: a Provider setting cannot create an
independent cron or background loop. During the first product surface, these
technical settings and per-Task overrides are visible and mutable only to
Master; ordinary users receive task, approval, execution, result and billing
surfaces without this administration panel.

## Storage baseline

The initial seven migrations cover system/outbox, identities, provider
accounts and encrypted secret references, tasks/snapshots/diffs/plans,
executions/attempts/leases/progress/logs, credits/pricing/entitlements, and
notifications/audit.

Cross-API identifiers use strongly typed UUIDv7 values. Remote task uniqueness
uses provider account, source type, and remote ID, with a separately versionable
fingerprint column for providers whose remote IDs are unstable.

## Executable surface

`asterismd` currently exposes:

- `GET /health`
- `GET /api/v1/system/health`
- `GET /api/v1/openapi.json`
- authenticated `/api/v1/providers` and auth/session/service-token routes

Every response receives an `x-request-id`. The initial CLI supports
`system health` and `provider list` against the same API.

## Second Phase 0 slice

The second checkpoint adds:

- guarded `Task` orchestration and `Execution` state transitions;
- an explicit `Recovering` state which requires remote verification after a
  crash instead of assuming failure;
- `assessment_class = Formal` execution/submission guards independent of an
  `Exam` source module;
- atomic SQLite execution leases with expiry, renewal, release, and concurrent
  acquisition tests;
- unified scan/execution/retry/notification job models with bounded backoff;
- zeroizing, redacted secret values behind a `SecretStore` interface;
- Argon2id password hashing and permission-based principals;
- leased transactional-outbox delivery with retry and dead-letter states; and
- startup recovery which preserves credit reservations and emits durable
  recovery-required events in the same transaction.

The health endpoint reports the schema version, registered provider count, and
pending/dead-letter outbox totals.

## Next Phase 0 slice

The third checkpoint adds atomic credit grant/reserve/commit/release operations,
immutable debit transactions, and same-transaction outbox events. Concurrent
reservations cannot overspend an account and a reservation cannot be committed
twice. It also adds leased scheduler job claiming/retry/dead-letter persistence,
256-bit opaque token generation with digest-only storage values, normalized
user roles, and a transactionally one-time initial Master bootstrap.

## Fourth Phase 0 slice

The fourth checkpoint persists server-side Web sessions and scoped service
tokens using digest-only token storage. It adds atomic expiry/revocation checks,
same-transaction lifecycle audit records, generic login failures, bounded login
rate limiting, scope-subset enforcement during token rotation, strict Cookie
attributes, no-store responses, authenticated Provider metadata, and complete
auth-route coverage in the internal OpenAPI document.

The daemon rejects non-loopback listeners while Phase 0 lacks native TLS and
explicit trusted-proxy configuration. Process-level tests cover Master
bootstrap, Cookie access, Bearer access, self-revocation, logout, and rejection
after either credential is revoked.

## Fifth Phase 0 slice

The fifth checkpoint establishes typed TOML configuration with deterministic
`CLI > environment > file > defaults` precedence. Local configuration is
ignored by Git, unknown keys and unsafe merged values fail at startup, and
credentials remain outside the ordinary configuration model.

The next slice adds authenticated CLI management flows without bypassing the
HTTP API.

## Sixth Phase 0 slice

The sixth checkpoint adds HTTP-only CLI flows for one-time Master bootstrap,
password login, current-identity inspection, Provider metadata, and scoped
service-token creation and revocation. Passwords use a hidden terminal prompt
or explicit stdin mode; service tokens are accepted only through the dedicated
process environment and are never regular configuration or command-line
arguments. Temporary Web sessions are revoked after issuing the requested CLI
token, and cleartext HTTP is rejected for non-loopback targets.

The next slice begins Provider account and task-management actions on the same
authenticated API boundary.

## Seventh Phase 0 slice

The seventh checkpoint adds owner-scoped Provider account lifecycle operations
through repository, API, and CLI layers. Ownership comes only from the
authenticated identity, every storage lookup includes the owner, responses
expose a credential count rather than Secret references, and create/update/delete
audits commit in the same transaction as the account mutation.

The next slice exposes owner-scoped Task queries while keeping remote and local
orchestration state distinct.

## Eighth Phase 0 slice

The eighth checkpoint adds owner-scoped, paginated Task reads through a
dedicated query repository, API, and CLI. Queries join through the owning
Provider account, optionally filter by account, use bounded offset pagination,
and preserve source type, assessment class, remote state, and orchestration
state as independent dimensions. Provider fingerprints remain internal, and no
manual Task write endpoint is introduced before Provider ingestion exists.

The next slice defines the Provider scan ingestion contract, including stable
fingerprints, sanitized snapshots, diffs, and transactional upserts.

## Ninth Phase 0 slice

The ninth checkpoint adds transactional Provider scan ingestion. Versioned
fingerprints provide a stable fallback when a remote identifier changes, while
conflicting remote keys and fingerprints fail closed. Each meaningful change
writes the normalized and sanitized remote snapshot, its typed diff, the Task
upsert, and a domain event through the transactional outbox in one commit.
Identical observations are idempotent, local orchestration state is preserved,
and bounded payload validation rejects credential-shaped fields before any
Course or Task row is written.

The next slice invokes this contract through registered Provider inventory
capabilities without introducing any concrete Provider implementation.

## Tenth Phase 0 slice

The tenth checkpoint adds a capability-driven Provider scan service. It accepts
only authenticated Provider accounts, passes opaque credential references in a
bounded runtime context, and collects the complete Course and Task inventory
before one repository write. Course-scoped results cannot escape their scope,
task capabilities must be a subset of Provider metadata, and Provider failures
never create a partial local observation.

`RemoteTask` now carries `assessment_class` and a normalized payload explicitly;
an `Exam` source still does not imply a formal assessment. This checkpoint adds
no concrete Provider and makes no availability claim.

## Eleventh Phase 0 slice

The eleventh checkpoint exposes owner-scoped manual scans at
`POST /api/v1/provider-accounts/{account_id}/scan` and through
`asterismctl provider-account scan`. The endpoint requires account-management
authority and an authenticated remote account, preserves sanitized Provider
error classes as bounded HTTP outcomes, and returns only aggregate scan results
plus changed local Task IDs.

Successful scan audits share the inventory transaction and retain the request
correlation ID, actor, Provider version, and aggregate counts. A failed Provider
call or rejected inventory writes neither partial Task data nor a successful
scan audit.

## Twelfth Phase 0 slice

The twelfth checkpoint connects claimed Scheduler scan jobs to the Provider
scan service. Runtime account lookup is internal and account-ID scoped; it does
not weaken owner-scoped management queries. A valid live worker claim is
required before any Provider call, and completion/failure updates retain that
claim owner.

Retryable storage, network, availability, and rate-limit failures use the shared
bounded exponential policy. A bounded Provider `Retry-After` can extend the
delay, while authentication, human-action, invalid-inventory, missing-account,
and unregistered-capability failures dead-letter without a retry loop. This is
an execution primitive for the unified Scheduler, not a Provider-owned loop.

## Thirteenth Phase 0 slice

The thirteenth checkpoint gives `scan_schedules` a typed repository contract and
atomically materializes due periods into idempotent Scan jobs. Materialization
uses an immediate SQLite write transaction so concurrent ticks cannot create
two jobs for the same schedule occurrence.

After downtime, one due job is created and `next_run_at` advances directly past
the current time instead of replaying every missed interval. Disabled schedules
do not materialize, the per-tick batch is bounded, and Provider-specific minimum
interval policy remains above this persistence primitive.

## Fourteenth Phase 0 slice

The fourteenth checkpoint adds a bounded unified scan-worker tick. Each tick
materializes due periods, atomically claims only `scan` jobs with
`UPDATE ... RETURNING`, and processes them sequentially through the classified
runner. Repeating the same worker and lease parameters cannot return an older
claim, and scan workers never consume execution or notification jobs.

Worker identifiers, claim TTLs, materialization batches, claim batches, and
retry policies are validated before use. The tick produces aggregate operational
counts and remains a lifecycle-neutral primitive for the daemon loop.

## Fifteenth Phase 0 slice

The fifteenth checkpoint starts the unified scan worker inside `asterismd` with
typed layered configuration. Scheduler enablement, tick interval, bounded
materialization/claim batches, claim TTL, and retry policy follow the same
`CLI > environment > file > defaults` precedence as the server and database.

HTTP graceful shutdown and the scheduler share one signal. Shutdown stops new
ticks, waits for an in-flight tick to return, joins the scheduler task, and only
then closes SQLite. Missed timer ticks are skipped rather than replayed, and an
individual tick failure is logged without terminating the long-running daemon.

## Sixteenth Phase 0 slice

The sixteenth checkpoint makes the optional Provider scan-frequency floor part
of versioned Provider metadata. The value is expressed in seconds, is visible
through the Provider metadata API, and is rejected during registration when it
is zero or cannot be represented by Scheduler persistence.

This remains a Provider constraint rather than a Provider-owned timer. The Core
schedule-management surface applies the floor to a user's desired interval in
the next slice.

## Seventeenth Phase 0 slice

The seventeenth checkpoint persists a user's desired scan interval separately
from the effective interval enforced by Core. Existing schedules are migrated
with identical desired and effective values, while new writes reject zero,
unrepresentable, or shorter-than-desired effective intervals.

Keeping both values allows a later Provider metadata revision to recalculate
the effective floor without silently replacing the user's preference.

## Eighteenth Phase 0 slice

The eighteenth checkpoint exposes owner-scoped scan schedule reads and updates
through the versioned API. Core resolves the effective interval as the greater
of the user's desired interval and the registered Provider floor, preserves
schedule identity across updates, and schedules the next run from the update
time without replaying an immediate backlog.

Management writes recheck ownership and append the sanitized audit record in
the same immediate SQLite transaction. Responses distinguish desired,
Provider-minimum, and effective values; disabling retains the schedule while
preventing job materialization.

## Nineteenth Phase 0 slice

The nineteenth checkpoint adds `asterismctl provider-account schedule get` and
`set` as the operational client for the same owner-scoped API. Operators supply
the desired interval explicitly, may persist a disabled schedule, and receive
the resolved Provider minimum and effective interval as structured JSON.

## Twentieth Phase 0 slice

The twentieth checkpoint implements the SQLite `SecretStore` boundary with
XChaCha20-Poly1305 authenticated encryption. A fresh 192-bit nonce protects each
version, while associated data binds ciphertext to its typed ID, owner,
purpose, version, and key ID so persisted metadata cannot be swapped silently.

The active 256-bit key and retained decryption keys live in an injected,
zeroizing keyring rather than SQLite. Secret reads, writes, rotations, and
deletions are owner-authorized, version-checked, size-bounded, and audited
without plaintext. The database migration backfills version one for existing
blobs; daemon key configuration and Provider-account credential attachment are
separate following slices.

## Twenty-first Phase 0 slice

The twenty-first checkpoint injects the SecretStore keyring into `asterismd`
from process environment only. An explicit active key ID and a comma-separated
set of base64-encoded 32-byte keys must appear together; no TOML field or CLI
argument can carry key material, and parser failures never echo it.

The API state retains the configured encrypted store for credential services,
while `/health` exposes only a boolean readiness signal. Running without a
keyring remains possible until a credential operation is requested, so existing
non-Provider administration does not require a placeholder key.

## Twenty-second Phase 0 slice

The twenty-second checkpoint replaces raw credential-reference strings with
typed `SecretId` values across Provider accounts, runtime contexts, and SQLite
decoding. Malformed persisted references now fail at the storage boundary, and
account metadata updates still cannot bypass the dedicated credential
repository contract.

## Twenty-third Phase 0 slice

The twenty-third checkpoint models non-secret Provider credential metadata as
`ProviderCredential`: an account-scoped `SecretRef`, independent `SessionKind`,
acquisition source, captured time, optional expiry, and update time. Validation
rejects non-Provider secret purposes and backwards lifecycle timestamps.

SQLite now persists this metadata beside the encrypted blob association and
enforces one live credential per account and purpose. Existing links are
backfilled as provider-specific manual imports without changing their secret
material; transactional lifecycle operations follow in the next slice.

## Twenty-fourth Phase 0 slice

The twenty-fourth checkpoint adds an account-scoped `ProviderCredentialStore`.
Creating a credential encrypts its plaintext, inserts the account association,
and appends secret plus credential audits in one immediate transaction.
Purpose uniqueness requires rotations to use optimistic secret and metadata
versions instead of silently accumulating competing live values.

Listing returns metadata only. Rotation updates ciphertext, key/version,
expiry, and association timestamps atomically; deletion removes both the blob
and its cascading link. Generic SecretStore mutation rejects Provider purposes
so callers cannot bypass these invariants, and deleting a Provider account now
removes only its attached encrypted blobs before removing the account.

## Twenty-fifth Phase 0 slice

The twenty-fifth checkpoint introduces a normalized in-memory
`CredentialBundle` for candidate Provider credentials. Its plaintext fields
are zeroized on drop, bounded and unique by purpose, and redacted from debug
output together with account hints. Candidates also carry their Provider,
session kind, acquisition source, and lifecycle timestamps without becoming a
serializable persistence model.

`AuthenticationCapability` now validates this candidate bundle through a
dedicated pre-persistence context. This establishes the contract that remote
Provider validation must succeed before Core may hand plaintext to the
account-scoped credential transaction.

## Twenty-sixth Phase 0 slice

The twenty-sixth checkpoint adds the validation-gated credential orchestration
service. It verifies owner access, exact Provider and tenant binding, advertised
authentication methods and session kinds, and a consistent successful Provider
status before persistence is called. Rejected or malformed Provider results
leave both credentials and account authentication state unchanged.

Successful validation replaces the complete encrypted credential set and marks
the account authenticated in one immediate SQLite transaction. Replacement
deletes superseded blobs through their account links, writes per-secret and
per-credential audits, and records one sanitized bundle commit audit without
persisting candidate account hints or plaintext.

## Twenty-seventh Phase 0 slice

The twenty-seventh checkpoint exposes owner-scoped full credential replacement
at `PUT /api/v1/provider-accounts/{account_id}/credentials`. Provider identity
and tenant are derived from the owned account rather than accepted from the
request, and both Web sessions and owner-bound service tokens retain their
typed audit identity through the secret boundary.

Credential values are converted directly into zeroizing memory wrappers,
redacted from request debug output, and never returned. The response contains
only credential count plus the sanitized Provider session status; OpenAPI marks
every value as write-only and documents validation, conflict, rate-limit, and
availability outcomes.

## Twenty-eighth Phase 0 slice

The twenty-eighth checkpoint adds
`asterismctl provider-account credential import`. The command accepts only
non-secret credential metadata as arguments: the value has no command-line
option and is read from stdin, with hidden terminal input for interactive use.

Piped values have one trailing line ending removed, are bounded before the HTTP
request, and remain in redacted zeroizing wrappers until the request finishes.
The CLI then drops the value explicitly and prints only the API's sanitized
commit result.

## Twenty-ninth Phase 0 slice

The twenty-ninth checkpoint models each Provider authentication attempt as an
owner- and account-scoped `AuthSession`. A session starts in `Starting`, carries
an independent `AuthMethod`, has a mandatory expiry, and increments a revision
on every observable state change.

The domain transition table permits waiting, exchange, validation,
authentication, refresh, expiry, and explicit exception outcomes without
allowing skipped validation, timestamp regression, revision overflow, or
revival after a terminal failure or cancellation. `SessionKind` remains outside
the process state and is produced only by validated credentials.

## Thirtieth Phase 0 slice

The thirtieth checkpoint persists owner-scoped authentication sessions with
their method, tagged state, expiry, timestamps, and optimistic revision. Session
creation and each legal transition mirror the Provider account authentication
state and append a sanitized audit record in the same immediate transaction.

Updates reload the current record under the write lock, replay the domain
transition, compare immutable fields, and require the exact prior revision.
Only the newest session for an account may advance, preventing a stale callback
from an older attempt from replacing the account's visible authentication
state. Migration 016 adds owner and account indexes for polling surfaces.

## Thirty-first Phase 0 slice

The thirty-first checkpoint adds Core orchestration for beginning and cancelling
Provider authentication. Core persists `Starting` before invoking Provider
code, passes that typed session ID in the authentication context, and accepts
only a challenge that echoes the same session and method with bounded public
user-action data.

A valid challenge advances to its explicit `WaitingUser` subtype. Provider
failures are classified and persisted as authentication failure, human action,
Provider unavailability, or client update requirement; an attempt that expires
during the call becomes `Expired`. Cancellation is owner-scoped, revisioned,
and cannot revive terminal or superseded attempts.

## Thirty-second Phase 0 slice

The thirty-second checkpoint exposes owner-scoped authentication state at the
Provider-account boundary: begin a server-TTL session, read the latest attempt,
read an exact account/session pair, and cancel it. Account and session IDs are
both checked so an owned session cannot be addressed through another account.

Begin returns the initial bounded Provider challenge once alongside the Core
session. Status and cancellation return only the persisted non-secret
`AuthSession`; every response is marked `no-store`, and OpenAPI describes the
conflict, Provider failure, expiry, and rate-limit outcomes. A later durable
Provider-native poll route is specified separately below.

## Thirty-third Phase 0 slice

The thirty-third checkpoint adds `asterismctl provider-account auth` commands
for starting, inspecting, and cancelling the same Core sessions. `status`
defaults to the latest attempt for an account and accepts an explicit session
ID when callers need a stable polling target.

These commands carry only authentication method and typed IDs. Credential
values remain on the separate stdin-only import path, preserving the boundary
between how authentication is performed and which validated session material
is ultimately stored.

## Thirty-fourth Phase 0 slice

The thirty-fourth checkpoint closes the Core authentication transaction around
credential validation. A candidate attached to a waiting session first moves
that exact attempt to `ValidatingCredential`; the Provider receives the Core
session ID, and rejection or protocol failure becomes an observable terminal
state without writing secret material.

After remote validation, one SQLite immediate transaction verifies that the
session is still the account's newest attempt at the expected revision, writes
the encrypted credential set, advances the session and account to
`Authenticated`, and appends their audits. A newer attempt therefore makes an
older network callback lose the transaction before any ciphertext or account
state can be committed.

## Thirty-fifth Phase 0 slice

The thirty-fifth checkpoint exposes the atomic completion boundary at
`PUT /api/v1/provider-accounts/{account_id}/auth-sessions/{session_id}/credentials`.
Both typed IDs remain owner-scoped, and the request reuses the write-only,
redacted credential bundle contract while binding Provider validation to the
explicit Core session.

The response contains only the authenticated session, sanitized Provider
status, and credential count. Reusing a terminal, expired, or superseded
session returns a conflict and cannot replace the existing ciphertext. The
account-level credential import route remains available as a compatibility
surface, while session-aware clients use the stricter completion endpoint.

## Thirty-sixth Phase 0 slice

The thirty-sixth checkpoint binds `asterismctl provider-account credential
import` to a required `--session` ID and sends its stdin-only value to the
session completion endpoint. The CLI can no longer report a validated import
without naming the observable authentication attempt that owns it.

Secret input behavior is unchanged: interactive terminals use a hidden prompt,
pipes accept one bounded line, no plaintext option exists, and output contains
only the server's sanitized session and credential metadata. The resulting
operator flow is start, submit, then poll the same session ID.

## Thirty-seventh Phase 0 slice

The thirty-seventh checkpoint models the short-lived Capture pairing lifecycle
as `AuthBootstrapSession`. It records the owner, Provider, optional account,
purpose, required bundled recipe version, timestamps, claim time, state, and
optimistic revision while enforcing a maximum fifteen-minute TTL.

An add-account session has no existing account binding; reauthentication and
repair require one. A session can be claimed exactly once and then completed or
failed, while live sessions may be cancelled or expired without revival. The
raw pairing token is deliberately absent from this serializable domain object;
only a token digest belongs in persistence, and token issuance remains at the
authentication boundary.

## Thirty-eighth Phase 0 slice

The thirty-eighth checkpoint persists Capture pairing sessions and their token
rotation in migration 017. Creation stores only a 32-byte digest of the
high-entropy pairing token after rechecking the active owner and optional
Provider-account binding.

Claim runs under one SQLite immediate transaction. It accepts only the exact
unexpired, unclaimed digest; clears that digest, stores an independently
generated Bootstrap access-token digest, advances the domain revision, and
appends a sanitized audit. Wrong tokens, expired tokens, and replayed tokens all
return the same empty result and cannot reveal or mutate session state.

## Thirty-ninth Phase 0 slice

The thirty-ninth checkpoint adds Core orchestration for pairing-token issuance,
claim, and owner cancellation. Creation returns the `ast_pair` plaintext only
after its digest is committed. Claim generates an independent `ast_boot` token,
rotates the digests transactionally, and returns that plaintext only when the
one-time claim succeeds.

Invalid, expired, cancelled, and replayed pairings deliberately share one
rejection outcome. Secret token wrappers redact every debug representation and
zero their allocations on drop. Owner cancellation is revisioned and clears
both pairing and access digests; an overdue cancellation persists `Expired`
instead of misreporting a live cancellation.

## Fortieth Phase 0 slice

The fortieth checkpoint lets a registered Provider declare an optional,
positive `capture_recipe_version`. Current Core additionally requires the
Authentication implementation to return the exact validated declarative recipe;
metadata alone cannot claim an executable Capture path. Session creation freezes
the implementation-backed version, and clients cannot choose or downgrade it.

A Capture recipe declaration is valid only when the same Provider advertises
and implements Authentication, the recipe method/session kind are advertised,
and its bounded HTTPS origins and credential sources pass registry validation.
Providers without a bundled recipe remain
usable through their other authentication methods but cannot start a Capture
bootstrap flow.

## Forty-first Phase 0 slice

The forty-first checkpoint exposes owner-authenticated creation, read, and
cancellation for Capture pairings plus the public one-time claim endpoint.
Creation derives the recipe version from registered Provider metadata and
returns the pairing plaintext once; later owner reads contain only the
serializable session.

Claim accepts only `Authorization: Bootstrap <pairing-token>`, has an isolated
per-session and per-IP rate limiter, and returns the scoped access plaintext
once with `no-store`. Missing, wrong, expired, cancelled, and replayed tokens
share the same 401 response and Bootstrap challenge. OpenAPI distinguishes this
temporary scheme from long-lived Web and service-token authentication.

## Forty-second Phase 0 slice

The forty-second checkpoint authenticates a Bootstrap access token only against
the session ID carried by the request path. The digest must belong to a session
that is still `Claimed`, unexpired, and backed by an active owner plus its
original optional Provider-account binding.

Pairing-token digests cannot pass this check, and an access token from one
session cannot authorize another. Cancellation, failure, completion, expiry,
owner deactivation, or account deletion all make access authentication fail
without disclosing which condition occurred. Core exposes one redacted
`AccessRejected` outcome for future event, stream, and credential endpoints.

## Forty-third Phase 0 slice

The forty-third checkpoint exposes the access-token guard on
`GET /api/v1/auth-bootstrap/sessions/{session_id}/stream`. The first transport
is a `no-store` JSON snapshot for HTTP polling; the same path-bound guard is
intended to precede later SSE or WebSocket upgrades.

Only the claimed session's `ast_boot` token can read its snapshot. Using that
token with another session ID, or using it after owner cancellation, returns
the same Bootstrap 401 response. This temporary authority remains separate from
the global Bearer middleware and cannot access any account, task, or system
route.

## Forty-fourth Phase 0 slice

The forty-fourth checkpoint models Capture-to-Core status updates as
server-timestamped `AuthBootstrapClientEvent` values with a positive monotonic
session sequence. Events carry only fixed variants, 0-100 progress, and bounded
safe stage or failure-code identifiers; arbitrary messages, client timestamps,
and credential material are not part of this channel.

The wire-level `authenticated` variant is deliberately represented in Core as
`ClientReportedAuthenticated`. It is diagnostic input only and cannot advance
an `AuthSession`, authenticate a Provider account, or write SecretStore without
the separate server-side credential validation path.

## Forty-fifth Phase 0 slice

The forty-fifth checkpoint persists Capture client events in migration 018.
Access-token authentication, active owner/account revalidation, contiguous
sequence checking, event insertion, and sanitized audit all run under one
SQLite immediate transaction, so cancellation cannot race a late event write.

Sequences begin at one and advance without gaps. A retry with the same sequence
and kind returns the first server-timestamped event, while changed content or a
gap is a conflict. Recording `ClientReportedAuthenticated` never mutates the
bootstrap session, Core authentication state, Provider account, or SecretStore.

## Forty-sixth Phase 0 slice

The forty-sixth checkpoint exposes the event transaction through
`AuthBootstrapService`. Capture access tokens are digested at the engine
boundary, then the repository atomically authenticates the session and records
the event. Plaintext access tokens never enter domain events or persistence.

The engine distinguishes a new event from an exact retry while returning the
first server timestamp for both. Invalid access remains a redacted rejection;
changed or non-contiguous sequences become an explicit conflict for transport
layers to map without weakening the repository invariant.

## Forty-seventh Phase 0 slice

The forty-seventh checkpoint exposes Capture status submission at
`POST /api/v1/auth-bootstrap/sessions/{session_id}/events`. The public route
accepts only the path-bound Bootstrap access token and a sequence plus fixed
event kind; Core supplies `received_at` and rejects client-provided timestamps
or unknown fields.

A new event returns 201, an exact retry returns 200 with the original timestamp,
and changed or gapped sequences return 409. Responses are `no-store`, pairing
tokens and cross-session access tokens return the same Bootstrap 401, and the
client-reported `authenticated` event still leaves Core state unchanged.

## Forty-eighth Phase 0 slice

The forty-eighth checkpoint makes successful account binding part of the
`AuthBootstrapSession` completion transition. An `add_account` session remains
unbound while awaiting or claimed and receives its Core-created account ID only
when the credential flow completes. Failed, cancelled, and expired add-account
sessions cannot retain a partial binding.

Reauthentication and repair sessions remain bound to their original account
throughout the lifecycle, and completion rejects any different account ID.
This gives the future credential transaction one domain transition that works
for both account creation and credential replacement.

## Forty-ninth Phase 0 slice

The forty-ninth checkpoint adds the atomic Capture credential commit boundary.
After Provider-side validation, one SQLite immediate transaction revalidates
the path-bound Bootstrap access digest, creates or snapshot-checks the Provider
account, replaces its encrypted credential set, marks the account authenticated,
and completes the bootstrap session while clearing its access digest.

Add-account and reauthentication commits share this boundary. Invalid or
expired access writes neither accounts nor secret blobs; stale account bindings
are conflicts; and any failure after account or credential work begins rolls
the entire transaction back. A successful commit is one-time because the
completed session no longer retains an access-token digest.

## Fiftieth Phase 0 slice

The fiftieth checkpoint adds server-side orchestration for Capture credential
submissions. The engine resolves the claimed session from its scoped access
token, constructs a Core-owned account identity for `add_account` or reads the
current bound-account snapshot, and invokes the registered Provider's real
credential validator before persistence.

Provider rejection leaves the claimed session retryable and writes no account
or secret. A valid status is normalized back into the credential bundle and
passed to the atomic bootstrap commit; successful completion seals the access
token, while an account change during validation is surfaced as a binding
conflict instead of committing against stale metadata.

## Fifty-first Phase 0 slice

The fifty-first checkpoint exposes the server-validated flow at
`POST /api/v1/auth-bootstrap/sessions/{session_id}/credential`. The route uses
only the scoped Bootstrap access token, has its own per-session/IP limiter, and
sets both the capture timestamp and `capture_tool` acquisition source on the
server instead of accepting those claims from Capture.

The response contains only the completed session, account ID, credential count,
and sanitized Provider status. Provider rejection returns a retryable conflict
without writes; success atomically creates or updates the account and then
invalidates the access token, so a replay receives the same redacted Bootstrap
401 and never exposes submitted credential material.

## Fifty-second Phase 0 slice

The fifty-second checkpoint introduces the Rust-only `asterism-capture` binary
as an outbound client rather than a daemon or inbound listener. Its initial
`doctor` command checks the public Core health endpoint with redirects disabled,
a bounded response body, and a fixed request timeout.

Capture server URLs require HTTPS and cannot embed credentials, paths, queries,
or fragments. Cleartext HTTP is disabled by default and can be enabled only by
an explicit development flag for loopback hosts, preserving local Phase 0
testing without weakening the public deployment boundary.

## Fifty-third Phase 0 slice

The Capture client now claims an Auth Bootstrap session with the one-time
pairing token in a sensitive `Bootstrap` authorization header. The response is
accepted only when the returned session ID and claimed state match the request.

The resulting `ClaimedCaptureSession` retains its short-lived access token only
inside a redacted, zeroizing secret value. This capability is exposed through
the Capture library but is not yet a standalone command, avoiding a partial
workflow that would consume a one-time pairing and immediately discard access.

## Fifty-fourth Phase 0 slice

A claimed Capture session can now poll the Core-owned session snapshot and
append bounded client status events through the HTTP fallback endpoints. Every
request reuses the in-memory session-scoped access token in a sensitive header,
and every response is checked against the current session ID.

Event sequence numbers start at one, advance only after an accepted response,
and remain unchanged after ambiguous transport failures so the same event can
be retried through the server's idempotent sequence contract. Returned events
must match the submitted sequence and canonical event kind before acceptance.

## Fifty-fifth Phase 0 slice

The Capture library now submits normalized credential candidates to Core for
Provider-side validation and atomic persistence. Local validation bounds field
count and size, rejects duplicate or non-Provider purposes, enforces Bootstrap
account metadata rules, and derives the Provider ID from the claimed session.

Credential JSON is serialized into an HTTP body whose owning allocation is
zeroized on drop. Response acceptance requires immutable session continuity, a
completed account binding, matching credential count and session kind, and a
valid Provider status; only then is the in-memory Bootstrap access token
cleared and the local snapshot advanced to completed.

## Fifty-sixth Phase 0 slice

The Capture binary now exposes a complete manual credential workflow without
accepting pairing tokens, tenant values, or credential plaintext in process
arguments. Interactive secrets use hidden terminal input; non-interactive
callers provide bounded lines through standard input in the declared field
order.

The workflow validates all non-secret options before claiming, polls the
claimed session, emits `client_ready`, `manual.input`, `credential_detected`,
and `validating` events, and derives Provider and account semantics from the
Core-owned session. Manual authentication choices are limited to imported or
assisted sessions; native password, QR, and external OAuth remain separate
Provider-owned flows.

## Fifty-seventh Phase 0 slice

Capture now applies the claimed session deadline to the remaining manual
workflow and handles local Ctrl+C without requesting owner-only server
mutations. Terminal reads run on detached input workers so cancellation and the
session deadline remain observable while a prompt is waiting; dropping the
workflow releases all zeroizing secret owners.

The local access token is explicitly cleared on expiry, a Bootstrap 401,
successful credential completion, or ordinary session drop. Authorization
headers and credential request bodies share zeroizing backing allocations with
the HTTP request, while response buffers are also zeroized after decoding.
Server-side cancellation remains an authenticated owner action and is observed
by Capture as access rejection rather than broadened local authority.

## Fifty-eighth Phase 0 slice

The first concrete Provider slice adds a Chaoxing crate with bounded, offline
parsers for independent course Work and Exam inventories. Stable task identity
includes source module, course, class, and remote task dimensions; Chapter Work
is not merged into this inventory. Exam rows are selected structurally so status
words inside scripts cannot create phantom tasks.

List-page state remains conservative. Unknown labels stay `Unknown`, Exam source
does not imply formal-assessment classification, and Work detail redirects are
followed before an unsubmitted item is treated as open or expired. Session-bound
route values are excluded from normalized snapshots and fingerprints.

This checkpoint is fixture-only: it does not register Chaoxing, advertise a live
capability, or claim current account compatibility. Native HTTP versus
BrowserBridge transport remains pending real-account validation.

## Fifty-ninth Phase 0 slice

Course discovery can now carry bounded Provider route facts into later
capability calls without placing them in sanitized metadata. The ephemeral route
context is omitted by Serde, ignored by scan persistence, redacted in `Debug`,
and zeroized on drop. Key/value count, shape, size, duplicates, and control
characters are rejected at construction.

This closes the runtime boundary needed by platforms such as Chaoxing, where a
course task request needs account-scoped route values such as `cpi`. These facts
remain available only during the in-memory course-to-task scan and cannot leak
into API payloads, fixtures, task snapshots, or audit records.

## Sixtieth Phase 0 slice

Chaoxing's independent Work and Exam parsers now compose behind a typed
`TaskInventoryCapability`. The capability validates Provider/account context,
requires one course-scoped call, reconstructs course/class identity from the
ephemeral route context, and returns no partial inventory if either module
fails.

Network behavior is isolated behind a Chaoxing transport contract. Response
bodies are bounded, redacted in diagnostics, and zeroized on drop; a future
adapter must resolve credentials through Core and discover a fresh Work `enc`.
The capability metadata remains `Development`, advertises only TaskInventory,
and at this historical checkpoint had no Capture recipe. The missing transport,
CourseInventory and Provider-specific Capture/BrowserBridge recipe remained
required implementation work before a complete Provider claim.

## Sixty-first Phase 0 slice

The shared networking crate now resolves field-level policy with the required
`account > Provider > global > built-in` precedence. Profiles model proxy,
request timeout, address-family selection, TLS profile, user agent, admission
rate, and retry backoff; malformed, credential-bearing, or unbounded values are
rejected before a client is built.

HTTP clients use rustls/WebPKI, HTTPS-only requests, explicit timeouts, optional
system or configured proxy behavior, and no automatic redirects. Providers must
therefore classify login and cross-origin redirects rather than inheriting an
unsafe global policy. Rate-limit enforcement, concurrency semaphores, persistence,
and API management remain separate follow-up slices; this checkpoint supplies
the validated client foundation needed by the first native Provider transport.

## Sixty-second Phase 0 slice

Chaoxing now has a Native HTTP inventory adapter behind an injected session
resolver. Cookie material must cross Core's secrets boundary for one operation,
is marked sensitive in request headers, is redacted from diagnostics, and is
zeroized by its owning session value. Responses use explicit status/error
classification, structural login-page detection, streamed size bounds, UTF-8
validation, and zeroizing page owners.

Exam requests are built directly from the course/class/`cpi` route context. Work
requests first load the current course page and accept only an HTTPS
`mooc1.chaoxing.com/mooc2/work/list` iframe whose unique course/class values
match and whose fresh `enc` is present; foreign hosts, userinfo, custom ports,
duplicates, and mismatches fail closed.

This adapter is not daemon-registered or live-verified. The Core credential
resolver, CourseInventory producer, and same-account Native-versus-Browser test
remain required before metadata can advance beyond `Development`.

## Sixty-third Phase 0 slice

Chaoxing now exposes an offline-verified `CourseInventoryCapability` around the
current web `courselistdata` shape. The parser keeps course and class dimensions
in stable identity, excludes unopened rows, bounds normalized metadata, and
accepts `cpi` only from a matching allowlisted HTTPS course entry. The account
route value remains in the non-serialized, zeroizing route context used by the
later Work and Exam scan.

Root and folder documents merge deterministically: identical rows deduplicate,
while conflicting or duplicate identities fail closed. Synthetic fixtures cover
the normalization and route-rejection invariants. The authenticated course-list
transport, Core credential resolver, Provider registry entry, and real-account
verification remain pending, so Chaoxing continues to report `Development` and
is not available to daemon scans.

## Sixty-fourth Phase 0 slice

The Chaoxing Native HTTP adapter now also supplies the course inventory
transport. One short-lived resolved Cookie session fetches the root
`courselistdata` response, the interaction page, and every discovered course
folder. Requests use the donor-observed form fields and interaction Referer;
automatic redirects remain disabled by the shared networking client.

Folder discovery accepts only canonical numeric identities, rejects root or
duplicate IDs, and enforces a fixed count and length bound. Any root, folder,
status, body, authentication, or parse failure aborts the whole inventory rather
than persisting a partial course set. Request construction and folder parsing are
fixture-tested, but no current account has verified this route or the reported
`uf` fingerprint behavior. Registry wiring and the Core credential resolver
therefore remain intentionally absent.

## Sixty-fifth Phase 0 slice

Provider authentication validation can now return an optional derived credential
replacement without writing to `SecretStore` directly. Core preserves the
candidate's Provider, tenant, authentication method, acquisition source and
capture time; it accepts only a metadata-declared target session kind, validates
the Provider status and replacement bundle again, and then performs the existing
atomic credential replacement. Invalid, duplicate, empty or unadvertised derived
credentials never reach persistence.

This closes the contract required by native flows such as Chaoxing
Password-to-Cookie: a Provider can return renewable username, password and Cookie
fields as one Composite session while the original request allocation is
zeroized. `provider_username` is now a first-class protected secret purpose, and
`credential_input` distinguishes native credential entry from QR, OAuth or
session import. The contract is integration-tested, but no Chaoxing
authentication implementation or daemon registration is claimed by this slice.

## Sixty-sixth Phase 0 slice

Sanitized Provider errors can now attach a typed `HumanRequiredReason`. The Core
authentication state machine preserves a supplied reason such as
`ImageCaptcha` instead of collapsing every interactive challenge into generic
manual intervention; older Providers which omit the reason retain the safe
fallback behavior.

This enables a native authentication boundary to stop accurately on captcha,
SMS, browser or confirmation gates before any blind retry. The reason is
diagnostic state only and does not broaden Provider authority or expose response
bodies; it is also the typed handoff into a separately bounded first-batch
Capture/BrowserBridge or automated donor-compatible challenge path.

## Sixty-seventh Phase 0 slice

Chaoxing now implements development-level native Password and ImportedCookie
authentication. Password fields use the donor-compatible
AES-128-CBC/PKCS#7 login encoding, and the native transport accepts only bounded
JSON plus bounded `Set-Cookie` pairs. Successful login is provisional until the
derived Cookie passes the authenticated root course-list request.

After revalidation, the Provider returns username, password and Cookie as a
Composite replacement through Core's derived-credential boundary; it never
writes secrets itself. Imported Cookie sessions use the same course-list check
without performing a password exchange. Captcha and SMS/secondary verification
stop as typed `HumanRequired` states and are not solved automatically.

Synthetic fixtures and donor-compatible encryption vectors cover the offline
contract. No real account has verified `fanyalogin`, Cookie persistence or the
reported `uf` TLS-fingerprint behavior, so Chaoxing remains `Development` and is
still absent from the daemon registry.

## Sixty-eighth Phase 0 slice

Provider runtime credential access now uses a Provider-scoped Core resolver
instead of exposing storage or owner lookup to Provider implementations. The
SQLite adapter resolves only an authenticated account whose Provider ID and
complete opaque credential-reference set match the runtime request, decrypts
inside one transaction, decrypts only explicitly requested credential purposes,
returns redacted and zeroizing values, and records an audit entry for every
resolved field. This reusable boundary is independent of Capture and does not
make any Provider live or verified by itself.

## Sixty-ninth Phase 0 slice

Chaoxing's runtime session resolver now consumes the Provider-scoped Core
credential boundary and selects exactly one non-expired Cookie or Composite
Cookie field for native authentication and inventory requests. Missing,
malformed, stale, wrong-Provider or incompletely bound credentials fail closed;
storage and decryption failures remain sanitized internal errors. Saved
username and password fields are retained for the separate auto-renew slice but
are never logged, returned by APIs, or used for an unbounded retry loop.

## Seventieth Phase 0 slice

The Chaoxing crate now has one explicit development factory which composes its
Password/ImportedCookie authentication, course inventory, and independent Work
and Exam inventory capabilities around the same Core session resolver and
resolved network policy. A registry validation test prevents advertised
metadata from drifting away from attached capability slots. The factory does
not register itself in `asterismd`, perform network calls during construction,
or change the Provider's `Development` verification level.

## Seventy-first Phase 0 slice

Provider-scoped runtime credentials now also expose a compare-and-replace
renewal boundary. A renewal must bind the authenticated account, Provider,
tenant and complete previous credential metadata, including each Secret
version, before one SQLite transaction can encrypt the replacement, remove the
prior blobs, update account references and emit sanitized audits. Concurrent or
delayed refreshes carrying stale metadata fail with a version conflict and
cannot overwrite a newer session or password.
This is a generic persistence contract only; automatic Chaoxing retry remains a
separate Provider transport slice.

## Seventy-second Phase 0 slice

Chaoxing now has a bounded Cookie-first renewal path which remains independent
of Capture. Missing or locally expired Cookies and remote authentication
responses may trigger one saved-password login, one root course-list Cookie
validation, one compare-and-replace credential commit, and one replay of the
whole inventory operation. Provider, account, full credential metadata and
Secret versions remain bound throughout; striped per-account renewal locks stop
same-process scans from issuing duplicate logins, while storage version
conflicts prevent stale writers across processes. Captcha or SMS responses stop
as typed human-required states, and no operation enters an unbounded retry loop.
One bounded ten-minute cache, keyed by account, the original complete reference
set and correlation ID, lets later Course/Work/Exam calls in that same scan use
the newly committed Cookie even though their immutable context still carries
the pre-renewal references; entries remain redacted and zeroize when evicted.
The development factory can compose this resolver and renewer directly while
sharing the same native authentication transport with session validation; it
still does not register Chaoxing in the daemon or imply live verification.

## Seventy-third Phase 0 slice

`asterismd` can now register the complete Chaoxing development entry only behind
an explicit local-validation setting. The default registry remains empty; the
opt-in refuses startup without a configured encrypted SecretStore, uses the
shared built-in network policy, logs an unverified-development warning, and
continues to expose `Development` metadata rather than a support claim. TOML,
environment and CLI settings follow the normal precedence model, contain no
credentials, and do not enable Capture.

## Seventy-fourth Phase 0 slice

`asterismctl provider-account credential import` now accepts an ordered,
repeatable set of credential purposes so a native login can submit one complete
candidate bundle. Chaoxing's Password flow can therefore collect Provider
username and password through separate hidden prompts, while ImportedCookie and
other single-field flows remain compatible. Non-interactive input holds one
stdin lock and consumes exactly one line per purpose, avoiding buffered-line
loss; duplicate purposes are rejected before any Secret is requested. Values
remain absent from process arguments, retain zeroizing storage through the HTTP
request, and are dropped immediately afterward. This completes the local
Password-to-Cookie validation surface but does not constitute live-account
verification or change Chaoxing's `Development` status.

## Seventy-fifth Phase 0 slice

Chaoxing Work inventory now rechecks every list-level pending item through its
current detail route before returning a scan. This closes the known ambiguity
where the list says `未交` after a task has already expired. Each short-lived
route is bound to the current course and Work identity, redacted in diagnostics,
limited by document, task, pending-detail, query and redirect budgets, and may
redirect only among the expected HTTPS Work routes on the fixed Chaoxing host.
The native adapter resolves one Cookie for the bounded batch and retries the
whole detail operation at most once after an authentication failure. Missing,
duplicate, foreign or unclassifiable detail results fail the complete task
inventory rather than returning a partially corrected snapshot; detail state is
included in the normalized fingerprint only after the full batch succeeds.
Script/style template keywords are removed before visible status
classification. Fixture and route-boundary coverage do not replace the pending
same-account browser comparison, so Provider verification remains
`Development`.

## Seventy-sixth Phase 0 slice

The Chaoxing Exam inventory regression set now includes the donor-observed
`ul.nav > li[data]` shape with an unclassified status span and a separate `.fr`
time label. Status extraction prefers the explicit status element, retains the
time text independently, and maps `未开始` to `NotOpen` instead of the former
`Unknown`. A separate synthetic unknown state remains in the fixture so new
vocabulary is still surfaced rather than guessed. Open/due/close timestamps are
not populated from unverified relative or ambiguous labels, and this parser
correction does not change the Provider's `Development` verification level.

## Seventy-seventh Phase 0 slice

Chaoxing TaskInventory now keeps Chapter as a third independent module beside
Work and Exam. The parser ports the primary `Samueli924/chaoxing` chapter-tree
shape: each `div.chapter_unit` child is bound by its `cur{knowledgeId}` identity,
`clicktitle`, bounded `knowledgeJobCount`, and completion or unlock marker.
Pending, completed and locked chapters map to separate normalized states; bad
identities, duplicate chapters, invalid counts and conflicting markers fail
closed. The native transport fetches the donor route
`mooc2-ans/mycourse/studentcourse` with the scan-local course, class and cpi
facts, using the existing Cookie-first bounded renewal behavior. Chapter, Work
and Exam documents must all succeed before a combined inventory is returned.
Fixtures and route tests remain offline evidence, so no execution capability or
higher verification level is advertised.

## Seventy-eighth Phase 0 slice

Chaoxing now has a bounded offline parser for the primary donor's
`knowledge/cards` chapter-resource document. It extracts the assigned `mArg`
JSON object without evaluating page scripts, treats donor-style empty card
slots as no tasks, and keeps video, document, reading, live and chapter-work
attachments distinct. Completed resources remain visible in inventory, while
unknown job types, malformed objects, duplicate identities and over-budget
documents fail closed. Stable task identities exclude the mutable resource
kind, and normalized or sanitized output excludes transient routing and
submission material such as `enc`, `jtoken`, `mid`, `otherInfo` and defaults.
Synthetic donor-shaped fixtures cover state, type, string-brace and redaction
behavior. Native card transport, integration into TaskInventory and every
resource execution capability remain separate work, so this slice makes no
live-support or verification claim. The parser itself requires no Capture, but
that does not exclude Capture/BrowserBridge paths required by later donor flows.

## Seventy-ninth Phase 0 slice

Chaoxing TaskInventory now expands each pending, unlocked Chapter which reports
at least one job through the primary donor's fixed HTTPS `knowledge/cards`
route. The native adapter requests card indexes 0 through 6 sequentially with
the scan-local course, class, knowledge and cpi binding, resolves one Cookie for
the bounded batch, and may replay the whole batch only once after an
authentication failure. A scan accepts at most 64 resource-bearing chapters,
seven documents per chapter, four MiB per document and 32 MiB across the batch.
Every requested card must be returned exactly once before parsing; missing,
duplicate, foreign or cross-card duplicate identities fail the full inventory.

Completed attachments discovered inside an active Chapter remain visible, but
fully completed and locked Chapters are not expanded into another seven network
requests. Resource Video, Document, Read and Live tasks retain `Resource`
source semantics, while donor `workid` attachments remain Chapter tasks and are
not merged with independent Work inventory. Chapter, Resource, Work, Work
detail and Exam acquisition must all complete before Core receives a snapshot.
The route and matrix tests are offline evidence only: no resource execution
capability, live-account verification or higher Provider verification level is
claimed. This native inventory slice does not by itself establish whether its
live session requires a BrowserBridge/Capture fallback.

## Eightieth Phase 0 slice

Chaoxing now implements the first task-level `ResourceExecution` subset for
pending Document and Read attachments. The Provider entry advertises and
attaches the generic execution slot, but only those two resource kinds receive
the corresponding task capability. Each execution rediscovers current Course
and cpi facts, rechecks the Chapter tree, fetches the exact seven-card matrix,
and binds one stable resource identity before any mutation. The attachment
`jtoken` exists only inside a redacted, zeroizing target; parsed JSON string
values are explicitly zeroed when their short-lived owner drops.

Document uses the donor-observed `ananas/job/document` GET and Read uses
`ananas/job/readv2`, with course, class, knowledge, job and fresh token bound to
the fixed Chaoxing origin. HTTP success is not completion: the capability
refetches all seven cards and returns `verified = true` only when that same task
reports completed. A task already completed on the first fresh read is
idempotent and performs no mutation. Video, Live, Chapter Work, independent
Work and Exam do not borrow this path and advertise no newly implemented
execution capability. Provider-to-Core execution dispatch and live-account
behavior remain unverified, so Chaoxing stays `Development`; the missing
Capture/BrowserBridge compatibility path remains active separate work.

## Eighty-first Phase 0 slice

Core execution requests now have a dedicated SQLite repository and persisted
idempotency scope/key. One immediate transaction moves the Task from its
expected orchestration state to `Scheduled`, creates the `Execution`, enqueues
the unified Scheduler job, appends the sanitized audit record, and writes the
durable execution-state event. A failure at any point rolls the whole request
back.

Replaying the same scoped key returns the original Execution without creating
another job, audit, or event; reusing it for a different Task fails closed.
Existing databases migrate without fabricating keys for historical rows. This
slice establishes the request-side transaction boundary only: worker-owned
Attempt, Progress, terminal-state persistence and Provider dispatch remain the
next Core execution slices.

## Eighty-second Phase 0 slice

Execution workers now persist a lease- and Scheduler-claim-bound Attempt
lifecycle. Starting an Attempt atomically moves both Execution and Task to
`Running`, creates the numbered Attempt, initializes Progress, writes sanitized
audit/log entries, and appends durable state/progress events. Repeating the
same start while the live worker still owns the unfinished Attempt is
idempotent.

Progress updates require the live Execution lease and a running Execution;
older or same-timestamp updates do not overwrite newer state or duplicate
events. Attempt completion updates Attempt, Execution, Task, Progress and the
claimed Scheduler job, releases the Execution lease, writes audit/log/outbox
records, and optionally enqueues the uniquely numbered Retry job in one
immediate transaction. Success additionally requires verified completed
progress, while failed, human-required and retry states retain a typed Provider
error class. Provider dispatch and lease renewal during long-running calls are
the next Engine slice.

## Eighty-third Phase 0 slice

The Engine now runs claimed Execution and Retry jobs through the registered
task-level Provider capability. It loads the persisted Execution, Task and
authenticated Provider account, acquires the Task-level Execution lease,
starts one Attempt, maps Provider progress into Core stages, and accepts
success only when the Provider returns both `verified = true` and remote
`Completed`.

Scheduler claims and Execution leases are extended before dispatch and renewed
together on a bounded heartbeat during long calls. Losing either ownership
stops local finalization so startup recovery can re-read remote state instead
of guessing. Network, availability and rate-limit errors use the shared retry
policy; authentication and explicit human requirements stop for user action;
protocol drift, unsupported tasks and invalid remote outcomes retain typed
failure classes. Formal assessment execution and submission remain disabled by
default and stop before the Provider mutation. SQLite-backed tests cover
verified success, numbered retry and the formal-assessment guard. Daemon worker
lifecycle and user-facing execution actions remain following slices.

## Eighty-fourth Phase 0 slice

Task execution now enters through one owner-scoped Core Action shared by HTTP,
CLI and future interaction surfaces. `POST /api/v1/tasks/{task_id}/execute` requires a
bounded `Idempotency-Key`; the Engine checks the owner scope before mutable Task
state so a transport retry still returns the original Execution after the first
request changed the Task to `Scheduled`. Reusing that key for another Task fails
closed, while the atomic repository remains the final concurrency boundary.

Only action capabilities (`ResourceExecution`, `SubmissionExecute`,
`DurationReport`, `Discussion`, or `Practice`) may be scheduled. Read,
inventory, parsing, resolution, build, verification, and BrowserBridge flags do
not imply executable behavior. Remote terminal states, invalid orchestration
states and formal assessments are rejected before any job is created. Web
Sessions are recorded as `WebUi`; scoped service tokens are recorded as `Cli`
or `Yunzai` without accepting a caller-supplied owner or source. The OpenAPI
document and `asterismctl task execute --idempotency-key` expose the same
contract. Daemon claim-loop integration remains the next slice.

## Eighty-fifth Phase 0 slice

`asterismd` now runs a dedicated Execution scheduler loop beside the existing
Scan loop. SQLite claims are kind-filtered: Scan workers can only take `scan`,
while Execution workers can only take `execution` and `retry`; notification or
future job kinds remain untouched. The Execution worker intentionally claims
one job at a time so a long Provider call cannot let later preclaimed jobs
expire while waiting in a local sequential batch.

The daemon composes the registered Provider runtime with the persisted
Execution, lease, account, Task and Scheduler repositories. Its initial claim
TTL is also the Execution lease TTL; the runner renews both on a one-third-TTL
heartbeat and preserves the shared bounded retry policy and formal-assessment
guard. Each tick reports success, retry, human-required, failure, deferral,
dead-letter and already-terminal outcomes without logging Provider payloads or
credentials. Shutdown stops new ticks but drains the in-flight Scan or
Execution tick before closing SQLite, so dropping a Provider future does not
become the normal shutdown path. Filtered claim, worker tick and dual-loop
lifecycle tests remain offline evidence and do not change Provider verification
status.

## Eighty-sixth Phase 0 slice

Execution status now has an owner-scoped read model for API, CLI and future UI
surfaces. `GET /api/v1/executions/{execution_id}` joins ownership through the
Execution's Task and Provider account, returning not-found for both absent and
foreign IDs. A single SQLite read transaction returns the Execution, its latest
structured `ExecutionProgress`, and Attempt history ordered by `attempt_no`;
history is bounded and every stored numeric or enum value is validated while
decoding.

The response exposes typed state, stage, percentage, current item, item counts,
Attempt result, Provider error class and sanitized Provider trace ID without
including execution logs or Provider payloads. `asterismctl execution get`
calls that same route using the existing `task_read` scope, and OpenAPI records
the stable operation. Paginated log history and a live SSE/WS LogStream remain
separate surfaces so detail reads cannot become unbounded polling payloads.

## Eighty-seventh Phase 0 slice

Execution log history now has a separate owner-scoped, bounded read surface.
`GET /api/v1/executions/{execution_id}/logs` validates a 1-200 page size and a
bounded offset, verifies ownership through Task and Provider account, and reads
the total plus one page in a consistent SQLite transaction. Entries are ordered
by timestamp and their internal log identity so pagination remains stable when
multiple lifecycle records share a timestamp.

Each decoded entry retains its typed level and stage, optional Attempt binding,
sanitized Provider trace ID and `metadata_sanitized` value. Raw Provider
payloads and internal row IDs are not exposed. `asterismctl execution logs`
uses the same API and OpenAPI operation with explicit pagination. This endpoint
is for bounded history and merged-record presentation; a live LogStream remains
a separate SSE/WS slice rather than repeated full-history polling.

## Eighty-eighth Phase 0 slice

Execution history now has an owner-scoped list surface for CLI and the planned
WebUI Executions page. `GET /api/v1/executions` joins ownership through each
Execution's Task and Provider account, optionally filters by `task_id`, and
returns a bounded 1-200 page plus the owner-filtered total. Results use stable
newest-first ordering by creation time and Execution identity, while absent
filters never broaden access beyond the authenticated owner.

The SQLite read model validates offsets before converting them for SQL, and the
HTTP layer rejects malformed Task IDs, unknown query fields and out-of-range
pagination before storage access. Responses are marked `no-store` and contain
only the Execution summary; progress, Attempt history and logs retain their
separate bounded detail surfaces. `asterismctl execution list [--task ...]`
uses the same OpenAPI-recorded operation rather than reading SQLite directly.

## Eighty-ninth Phase 0 slice

Chaoxing Document and Read resources now expose a read-only
`TaskProgressRead` capability beside their existing native execution path.
Progress reads rediscover the current course route, Chapter request and all
seven bounded card slots, then locate the exact resource identity and normalize
only its current remote state and percentage. They never call the completion
endpoint and never persist the short-lived card token.

Pending Document and Read tasks advertise both progress read and resource
execution; already-completed resources retain progress read while dropping the
mutable action. Video, Live and Chapter Work still advertise neither capability
until their separate implementations exist. The Provider remains Development
and disabled by default: fixture-backed remote-state reads establish a safe
input for Core crash recovery, not real-account compatibility.

## Ninetieth Phase 0 slice

Crash recovery now closes the loop through a dedicated Scheduler job rather
than leaving `Recovering` as a terminal holding state. Startup and every daemon
Execution tick scan only Running executions whose Task lease is absent or
expired. One SQLite transaction moves Execution and Task to Recovering,
cancels their stale pending/claimed Execution or Retry jobs, inserts one
idempotent Recovery job, removes expired leases and emits the durable
recovery-required event. Recovery jobs share the serial Execution worker and
the same renewable Scheduler claim plus Task-level lease.

The Recovery runner uses only the registered `TaskProgressRead` capability. A
fresh remote Completed state closes the abandoned Attempt as succeeded without
calling execute; a fresh Pending state closes it and creates the uniquely
numbered Retry job. Remote InProgress and transient network/rate-limit/service
errors retry only the bounded verification job. Unknown, removed, expired,
unauthenticated, unsupported or retry-exhausted outcomes become
HumanRequired, preserving uncertainty instead of guessing failure. Finalizing
the abandoned Attempt, Execution, Task, structured progress, Recovery job,
lease, audit, log, outbox events and optional execution Retry is one immediate
transaction. Existing credit reservations remain preserved; attaching quote
and reservation creation plus success-commit/failure-release to the unified
execution action is still a separate billing integration slice.

## Ninety-first Phase 0 slice

The daemon now drains committed transactional-outbox records through a bounded,
leased dispatcher into the shared in-process `EventBus`. Dispatch runs
independently of Scheduler enablement, claims at most 128 records per tick, and
completes an in-flight batch before SQLite shutdown. The live bus is the current
sole sink: absence of a subscriber is a successful ephemeral delivery because
the database read models remain the synchronization source. Future durable
notification or integration consumers require their own per-consumer delivery
state rather than reusing this global live-delivery marker.

`GET /api/v1/executions/{execution_id}/stream` is an authenticated,
owner-scoped SSE surface. It subscribes before verifying and loading the
bounded Execution detail, sends that detail as the initial `snapshot`, then
filters the shared bus to state, progress, recovery-required and human-required
events for only that Execution. A lagged broadcast receiver emits an explicit
`resync` frame; reconnect and resynchronization use the existing detail and
paginated log-history endpoints instead of pretending that the in-memory bus
offers durable `Last-Event-ID` replay. The response disables caching and proxy
buffering, emits keep-alives, and observes the daemon shutdown signal so a live
stream cannot hold graceful shutdown open. Detailed `ExecutionLogEvent` live
fan-out remains a separate slice; this stream does not poll or expose raw
Provider payloads.

## Ninety-second Phase 0 slice

Persisted Execution lifecycle logs now enter the transactional outbox as typed
`ExecutionLogged` domain events in the same transaction that inserts their
history row. Starting an Attempt and finishing either a normal or recovery
Attempt therefore cannot expose a live log without its durable history, or
commit history without the corresponding live-delivery record. Repeating an
idempotent Attempt start still creates neither a duplicate log row nor a
duplicate event.

The owner-scoped Execution SSE filter now emits those envelopes as
`execution_log` frames alongside state, progress, recovery and human-required
events. Each frame retains the Core `ExecutionLogEvent` shape and its outer
domain-event ID and correlation ID, so consumers can merge live entries with
the bounded chronological history without receiving unrelated executions.
The initial frame remains a bounded Execution detail snapshot rather than an
unbounded log dump; reconnect and lag recovery continue through the paginated
log API. Provider-authored intermediate diagnostic logs need an explicit
validated sink before they can use this path; this slice streams only logs
already owned and sanitized by Core lifecycle persistence.

## Ninety-third Phase 0 slice

The task-execution Provider contract now receives one Core-owned
`ExecutionEventSink` for both normalized progress and intermediate structured
logs. Provider logs carry a Core log level, Provider-local stage label, bounded
single-line message, optional trace ID and optional sanitized metadata. The
contract rejects control-bearing or oversized text, metadata above eight KiB,
and recursively normalized credential-shaped keys before Engine stage mapping.
The SQLite repository repeats these checks as the final persistence boundary.

Appending a Provider log requires the current worker's live Execution lease, a
matching unfinished Attempt whose Execution is still Running, and fewer than
1000 existing log rows for that Attempt. Core supplies the timestamp and maps
the Provider stage to `ExecutionStage`; the history row and `ExecutionLogged`
outbox envelope commit together in one immediate transaction. Sink persistence
failure propagates through the Provider call instead of silently presenting an
unrecorded diagnostic as delivered. The Chaoxing Document/Read implementation
now reports bounded submit, verify, verified and already-completed lifecycle
messages through this sink without exposing route facts, completion tokens or
raw responses. This is fixture-backed observability and does not change the
Provider's Development verification status.

## Ninety-fourth Phase 0 slice

Credit reservations now fail closed unless their User, PriceQuote, Execution,
Task and amount form one exact persisted chain. Reservation verifies that the
Execution was requested by the same User, references the same Quote, and shares
the Quote's Task and immutable amount before moving any available balance. A
new unique Quote index prevents one Quote from funding multiple reservations,
while the existing unique Execution constraint continues to prevent multiple
reservations for one run.

Settlement revalidates the same chain instead of trusting historical foreign
keys alone. Commit is permitted only after the bound Execution has reached
Succeeded; release refuses a succeeded Execution, preventing an accidental
refund after verified completion. Corrupted or cross-bound legacy rows stop as
a credit invariant failure without changing reservation, balance, ledger or
outbox state. This hardens the standalone ledger primitive first; creating the
Quote, reserving credit and scheduling the Execution in one Core transaction,
plus settling it in the worker's terminal transaction, remain the next billing
integration slices.

## Ninety-fifth Phase 0 slice

Execution scheduling now accepts an optional, already-resolved billing bundle
and commits its immutable PriceQuote and active CreditReservation in the same
immediate transaction as the Task state transition, Execution, Scheduler job,
request audit and durable events. The bundle must bind exactly to the requested
User, Task and Execution; its amount, revision and reason are bounded before
persistence. A dangling Execution quote or a reservation without the matching
Execution quote is rejected before any write.

Balance reservation happens before a job can become visible. Insufficient
credit therefore rolls back the Task transition, Quote, balance mutation and
all execution-side rows together. Idempotency replay returns the original
Execution before any billing mutation, so one request cannot reserve twice.
Zero-cost entitlement coverage still creates a Quote, Reservation, audit
attribution and `CreditReserved` event, while lazily materializing the user's
zero-balance account. Current API execution requests explicitly use the
unbilled path until Pricing and Entitlement resolution is connected; Core does
not invent a price or hard-code a role bypass. Commit on verified success and
release on terminal failure/cancellation remain the next worker-transaction
slice, while recovery and HumanRequired continue preserving active reserves.

## Ninety-sixth Phase 0 slice

The worker finish transaction now settles any bound active reservation after
moving Execution and Task state, but before committing progress, Scheduler job,
lease release, audit, logs and outbox events. Succeeded commits the reserve,
removes it from the reserved balance and writes one immutable negative
`TaskExecution` ledger transaction. Failed or Cancelled releases the reserve
back to available credit without inventing a debit. Both paths emit the matching
credit event with the Execution finish correlation ID.

Settlement reuses the same full User/Quote/Execution/Task/amount validation as
the standalone ledger boundary and also requires the newly persisted terminal
Execution state. Missing, inactive or corrupted billing rows fail closed. If a
balance invariant prevents settlement, every earlier finish mutation rolls
back: the Attempt remains active, Execution and Task remain Running, the
Scheduler claim and lease remain owned, and no terminal audit or event becomes
visible. RetryWaiting, Recovering and HumanRequired are deliberately not
settled because the remote outcome is not yet final. Recovery success reaches
the same commit path through the shared finish transaction; uncertain recovery
continues preserving the reserve for later reconciliation.

## Ninety-seventh Phase 0 slice

Provider scan-schedule reads and writes now use the Master runtime-settings
authorization boundary instead of ordinary owner account management. A Web
session must resolve `ManageSystem`, which is granted to Master but not User or
Operator roles. Automation uses an owner-bound service token with the explicit
`ProviderManage` scope; read-only Provider tokens cannot inspect these settings.
The existing owner binding remains in force, and OpenAPI now identifies both
scan-schedule operations as Master-managed surfaces.

This is the first enforcement step for the runtime-settings control plane.
Provider defaults, optional ProviderAccount settings and Task overrides still
require a versioned schema and persistence model; until those exist, Core keeps
only the already bounded scan-schedule field rather than accepting arbitrary
Provider JSON. Ordinary account, authentication, Task and Execution surfaces
retain their existing owner permissions.

## Ninety-eighth Phase 0 slice

Authenticated owners can now read their credit account, immutable transaction
ledger and reservation history through bounded no-store API surfaces. A missing
account is represented as zero available and reserved credit without creating a
row during a read. Transaction and reservation lists use explicit limit/offset
bounds and stable reverse-chronological ordering; malformed or unbounded query
parameters are rejected before storage access.

Reservation history returns the immutable PriceQuote beside each reservation,
including the original amount, pricing revision and reason. The storage read
model revalidates User, Quote, Execution, Task and amount bindings for active
and settled history before exposing it, rather than trusting foreign keys or
returning a detached quote. Web access requires `ReadOwnCredits`; automation
requires an owner-bound `CreditRead` service token. The routes do not expose
another user's ledger and do not add grant, adjustment, pricing or arbitrary
settlement controls.

## Ninety-ninth Phase 0 slice

The Provider contract now owns a bounded, versioned runtime-settings schema
without adding those technical controls to ordinary Provider metadata. A field
declares its stable key, display text, constrained value kind, safe default and
the Provider, ProviderAccount or Task scopes where a Master override is valid.
Supported values are limited to booleans, bounded stepped integers, thousandth
fixed-point decimals, bounded stepped durations and closed choices; arbitrary
strings and credential-shaped free-form JSON are not part of the contract.

Registry admission rejects unversioned, duplicate, malformed or unbounded
schemas before a Provider becomes available. Resolution starts from each safe
Provider default and applies validated Provider, ProviderAccount and Task
patches field by field in increasing specificity, requiring an exact schema
version at every layer. This slice establishes the validation and deterministic
merge boundary only: Core persistence, revision/audit writes, Master-only API
surfaces and immutable Execution snapshots remain separate transactional
slices. The current Chaoxing implementation advertises an empty schema until a
corresponding execution capability actually consumes a concrete setting.

## One-hundredth Phase 0 slice

Core now persists validated Provider runtime-setting patches at Provider,
ProviderAccount and Task scope. The SQLite model uses partial unique indexes for
one active patch per exact target and composite foreign keys to bind Task to its
real ProviderAccount and ProviderAccount to its real Provider ID. Cross-Provider
or cross-account writes return target-not-found without creating a detached
configuration. Account and Task settings cascade with their owning resources;
the global Provider default remains independent.

Writes run under one immediate transaction, require an expected revision, and
advance a nonzero monotonic revision. Stale creates and updates return an
explicit revision conflict. The Provider schema validates every patch before
the transaction, while persisted JSON is capped at 64 KiB and separately bound
to its schema version. Each successful create or replacement writes a sanitized
Audit record in the same transaction containing only target identifiers,
schema/revision and changed field keys—not setting values. Master-only API
surfaces, effective-value/source reads and immutable Execution snapshots remain
the next integration slices.

## One-hundred-and-first Phase 0 slice

The internal API now exposes a dedicated administrative runtime-settings
control plane. A Master Web session can read the registered schema, configure a
global Provider default, configure any ProviderAccount override and configure a
specific Task override. An owner-bound service token with `ProviderManage` may
automate only its owner's account and Task overrides; it cannot read or mutate
the system-wide Provider default. User and Operator Web roles remain rejected,
and the ordinary Provider, account and Task responses do not contain technical
settings or their schema.

Each PUT carries the exact schema version and expected stored revision. Invalid
or out-of-range typed values fail before persistence, first writes return 201,
stale revisions return 409, and inaccessible targets remain indistinguishable
from missing ones. Each GET returns the Provider schema, all applicable stored
layers, the resolved values and a per-field source map identifying schema
default, Provider, ProviderAccount or Task. Scan-schedule administration now
uses the same authority model, so a Master is system-wide while automation
remains owner-bound. Execution creation still needs the next transactional
slice to freeze these resolved settings and layer revisions into the run.

## One-hundred-and-second Phase 0 slice

The atomic Execution scheduling boundary can now persist one immutable runtime
settings snapshot beside a newly created Execution. The snapshot contains the
registered Provider ID, schema version, resolved typed values, per-field source,
the three optional override revisions and a capture timestamp equal to the
Execution creation time. It is inserted in the same immediate transaction as
the Task transition, Execution, optional billing reservation, Scheduler job,
Audit record and Outbox event. Idempotent replay continues returning the first
Execution and therefore its original snapshot.

Storage rejects zero revisions, mismatched value/source keys, a source without
its required layer revision, oversized serialized maps, timestamp drift and a
Provider ID that does not match the Task's actual ProviderAccount chain. A
binding failure rolls back all scheduling mutations. Reads rejoin the snapshot
through Execution, Task and ProviderAccount and repeat the Provider and
lifecycle validation before returning it. Request Audit includes only Provider,
schema and revision attribution, never setting keys or values. Legacy and
explicitly unconfigured internal callers may still omit a snapshot; connecting
the standard Core execution Action to the settings resolver is the next slice.

## One-hundred-and-third Phase 0 slice

The standard owner-scoped Task execution Action now resolves the registered
Provider through the Task's actual ProviderAccount, loads the three applicable
runtime-setting layers and passes their final values, sources and revisions to
the atomic scheduling boundary. Callers cannot supply or omit this snapshot.

The SQLite transaction re-reads every layer and compares the exact schema,
patch contents and revision before creating an Execution. A concurrent Master
write therefore returns a conflict without changing Task state, reserving
credit, inserting an Execution or exposing a Scheduler job. Idempotent replay
continues to return the original Execution and its original settings snapshot.

## One-hundred-and-fourth Phase 0 slice

The task-execution Provider request now carries the immutable, versioned
runtime-settings snapshot selected for that Execution. Worker dispatch reloads
it by Execution ID, rechecks the Provider binding and validates the complete
value set against the currently registered schema before any remote mutation.
Missing or incompatible settings fail closed as an internal execution failure.

Retries reuse the same snapshot even after a Master changes Provider, account
or Task overrides. Providers receive typed read-only accessors rather than a
database handle or arbitrary JSON, preserving Core ownership of persistence,
scope resolution and audit.

## One-hundred-and-fifth Phase 0 slice

Chaoxing Video cards now use a native task-level execution path derived from
the audited `Samueli924/chaoxing` flow. Each attempt rediscovers fresh card
metadata, loads bounded media status, emits monotonic signed progress reports
after real waits and accepts success only after a complete fresh Card scan marks
the same stable resource passed. Playback rate and report interval come from
the frozen Master settings snapshot.

Object IDs, report tokens, `otherInfo`, face/attendance fields and signature
inputs remain short-lived zeroizing execution material. Captcha responses stop
at this native boundary as typed `HumanRequired(ImageCaptcha)` before any blind
retry. The audited donor solver and Capture/BrowserBridge compatibility route
remain active first-batch work rather than a deferred phase. This is offline
fixture and transport-boundary evidence only, so Chaoxing remains disabled by
default and `Development`.

## One-hundred-and-sixth Phase 0 slice

Provider setting definitions can now attach one optional portable Core behavior
without surrendering control of their key, label or safety range. Registry
validation rejects duplicate behaviors, incompatible kinds, unsupported scopes
and concurrency limits above Core hard caps. Schemas without a behavior retain
conservative scheduling defaults.

This contract prevents the Scheduler from matching names such as
`chaoxing_video_threads`. The first behaviors describe Provider execution
concurrency and ProviderAccount execution concurrency; the resolved values are
read from each immutable Execution snapshot.

## One-hundred-and-seventh Phase 0 slice

The Execution worker now claims and polls a bounded batch concurrently. Before
a Provider execution or recovery request reaches the network, one atomic
in-process admission controller checks Global, Provider and ProviderAccount
counts together. Waiting executions keep both Scheduler claims and Execution
leases alive, and dropping the guard releases all three counters together.

The deployment-wide global ceiling is a typed Scheduler configuration field;
Provider and account values cannot bypass it. Chaoxing declares separate
Provider-only and account-overridable concurrency settings. Both default to one
until real concurrent account behavior is verified; the account field may be
overridden for one ProviderAccount or one Task and remains frozen per Execution.

## One-hundred-and-eighth Phase 0 slice

The portable behavior contract now also supports a bounded ProviderAccount scan
interval. It must be a duration available at exactly Provider and
ProviderAccount scope, between one minute and seven days; Task overrides are
rejected because account inventory is the scheduling unit. The Provider still
cannot create a timer or background loop.

When Master configures an account Scan Schedule without an explicit interval,
Core resolves the current Provider default and account override, applies the
Provider metadata floor, then persists and audits the resulting Schedule through
the existing unified Scheduler. An explicit interval continues to override that
configuration operation. Chaoxing declares a 300-second to 24-hour range with
an 1800-second schema default; ordinary users do not receive this field.

## One-hundred-and-ninth Phase 0 slice

Chaoxing now registers a `TaskDetail` capability which treats stored task data
as a lookup key rather than current remote truth. The capability parses the
stable module/course/class/task identity, rediscovers the authenticated account's
course inventory, selects exactly one matching course through non-serialized
route context, and reruns the complete bounded Task inventory for that course.

The exact remote Task must still exist exactly once. A disappeared course or
Task becomes `RemoteChanged`; malformed or duplicate identity becomes protocol
drift. Returned normalized detail contains only sanitized task facts and no
`cpi`, Work `enc`, final URL or credential material. Work receives the existing
followed detail-state classification, while Exam remains conservative fresh
list evidence until dedicated current detail fixtures are added. This is still
offline evidence and does not raise Chaoxing above `Development`.

## One-hundred-and-tenth Phase 0 slice

The registered `TaskDetail` capability is now reachable through one owner-scoped
Core read service, `GET /api/v1/tasks/{task_id}/detail`, and
`asterismctl task detail`. Core resolves the Task's actual ProviderAccount,
requires an authenticated account and passes only opaque credential references
plus the bounded request correlation ID to the Provider.

Before returning a response, Core requires the fresh remote identity and source
type to match the stored Task, rejects capabilities absent from Provider
metadata, caps the complete detail payload and rejects credential-shaped keys in
every sanitized JSON region. Provider rate limits, temporary failures, remote
changes, required user action and protocol drift retain distinct stable HTTP
classifications. The read is not persisted as a scan snapshot and does not
change local orchestration state.

## One-hundred-and-eleventh Phase 0 slice

Chaoxing progress reads now dispatch by stable task module instead of treating
all tasks as Chapter resources. Executable Resource tasks retain the targeted
course/chapter/card rediscovery used by execution recovery. Chapter, independent
Work and Exam tasks reuse exact fresh `TaskDetail` rediscovery, including Work's
followed final-route state.

The latter modules advertise `ProgressRead` and expose only conservative binary
completion: `Completed` is 100, `Pending` is 0, and every other state has no
percentage. They never infer duration, score or partial progress from localized
list text. Foreign identities and mismatched detail results fail as protocol
drift, and all evidence remains offline-only until live-sanitized fixtures are
recorded.

## One-hundred-and-twelfth Phase 0 slice

Fresh `TaskProgressRead` is now reachable through one owner-scoped Core service,
`GET /api/v1/tasks/{task_id}/progress`, and `asterismctl task progress`. Core
requires the stored Task to advertise `ProgressRead`, resolves its actual
ProviderAccount, requires authenticated state and passes only opaque credential
references to the registered Provider capability.

The transport returns stable Task, Provider and implementation-version context
with the fresh normalized progress. Percentages above 100 fail at the Core
boundary. Missing task/provider capabilities, remote changes, user action, rate
limits, temporary failures and malformed Provider results receive distinct
stable API classifications, and the read neither persists a scan snapshot nor
changes orchestration state.

## One-hundred-and-thirteenth Phase 0 slice

The first-batch delivery order prefers Native Rust HTTP, but explicitly includes
Provider work that requires a local Capture runtime, browser traffic capture,
browser storage/runtime state, dynamic crypto context, local authentication
assistance or a system proxy. This is an engineering priority order only:
Native HTTP, BrowserBridge and Capture are all current implementation scope.

The generic Capture pairing and credential boundaries must be expanded with the
Provider-specific recipes and interception logic required by audited donor
behavior. In particular, cidaren's WeChat-assisted Capture path, runtime crypto
context and resulting question/submission abilities continue after the existing
ImportedToken checkpoint. They remain subject to owner/account binding,
zeroization, bounded capture and fail-closed drift handling.

## One-hundred-and-fourteenth Phase 0 slice

The Domain now defines persistence-independent `Question` and
`AnswerCandidate` contracts before any Provider-specific question parser is
registered. Questions retain Task binding, optional attempt-local remote ID,
type, one-based position, normalized stem, options and attachment facts without
carrying fetch URLs, signatures or Provider submission payloads.

Normalized answers distinguish selections, texts, booleans, pairs, ordering and
composite structures. `AnswerSource` remains independent from Provider parsing,
and optional confidence uses validated basis points instead of floating-point
ambiguity. Domain validation bounds collection sizes and nesting, rejects
duplicate option/answer identities and credential-shaped metadata, and prevents
one Provider parser from smuggling raw secrets into later answer or submission
layers.

## One-hundred-and-fifteenth Phase 0 slice

`QuestionInventory` and `QuestionParse` now have independent Provider trait and
registry slots. Advertising either read capability no longer forces a Provider
to attach `TaskExecutionCapability`, so question discovery/parsing cannot
silently imply permission or implementation for remote mutation.

Inventory returns bounded `RemoteQuestionRef` values with stable attempt-local
identity, position, type hint and sanitized facts. Short-lived Provider route
context is explicitly skipped during serialization, as with course routing.
Parsing receives the Core-owned local Task ID and one fresh reference and must
return the normalized Domain `Question`; later Core orchestration will validate
the complete question before persistence or answer resolution.

## One-hundred-and-sixteenth Phase 0 slice

Core now owns an owner-scoped, all-or-nothing Question read service. A stored
Task must explicitly advertise both `QuestionInventory` and `QuestionParse`;
Core then resolves the authenticated ProviderAccount and invokes only the two
registered read slots with opaque credential references. Formal assessment
tasks pass through the existing `Parse` guard, which permits reading without
granting execution or submission authority.

The complete reference set is bounded, deduplicated by remote identity and
position, and sorted deterministically before parsing. Every returned Question
must match the Core Task, fresh reference identity, position and non-unknown type
hint, then pass Domain bounds and sanitization. Nothing is returned when any
question fails, and no question or answer state is persisted by this slice.

## One-hundred-and-seventeenth Phase 0 slice

The complete fresh Question read is now exposed as owner-scoped
`GET /api/v1/tasks/{task_id}/questions` and `asterismctl task questions`. Both
surfaces require Task Read permission and preserve the Core service's
all-or-nothing result; HTTP responses are `no-store`, and Provider failures use
the stable rate-limit, availability, user-action, remote-change, capability and
invalid-response error classes already shared by fresh Task reads.

The route is documented in the generated OpenAPI surface and rejects malformed
Task IDs before any Provider access. It remains a read-only inspection boundary:
no Question persistence, answer lookup, draft creation or submission authority
is implied by this API.

## One-hundred-and-eighteenth Phase 0 slice

The Chaoxing Provider now has deterministic offline parsers for the locked
CxKitty mobile Chapter Work and Exam structures and the current OCS independent
Work/Exam preview taxonomy. Synthetic sanitized fixtures keep those page modes
distinct and cover
attempt-local identity, ordering, choice options, extended Question types and
non-fetchable attachment labels. The parser rejects duplicate identities,
missing supported structure, login documents and unbounded input instead of
returning an incomplete set.

This parser is intentionally not yet registered as a live Provider capability.
It never reads hidden/current answer fields, fetch URLs or submission-form
tokens, and emits only Domain Question data plus short-lived Question
references. Native transport and the all-or-nothing attempt cache remain a
separate following slice, so offline fixtures are not presented as real-account
verification.

## One-hundred-and-nineteenth Phase 0 slice

The Chaoxing Question audit now explicitly corrects a donor-boundary ambiguity:
CxKitty's mobile `mworkspecial` parser describes Chapter Work, not the
independent course Work inventory. Independent Work and Exam use the current OCS
`.questionLi` preview taxonomy and separate fixtures. Parser entry points and
metadata preserve this distinction so later native transport cannot route an
independent Work Task through Chapter attachment credentials.

All supported page modes remain parse-only and answer-free at this historical
checkpoint. The correction itself adds no remote execution or submission
behavior; those audited paths and any required Capture/BrowserBridge dependency
remain active later capabilities, while hidden donor answer behavior is handled
only through a distinct `AnswerResolve` contract.

## One-hundred-and-twentieth Phase 0 slice

Chaoxing now registers native `QuestionInventory` and `QuestionParse` for
independent Work only. A Work Task advertises both capabilities only after its
fresh detail redirect confirms the pending editor state. The reader then
rediscovers the exact course and Work entry, follows the existing fixed-host
redirect boundary, requires the final `/work/dowork` route and parses one
bounded response through the independent Work preview grammar.

Core inventory and parse calls share normalized Questions through a bounded
five-minute process-local cache keyed by ProviderAccount, correlation ID and
stable Task identity. It is removed after the final Question and never persists
HTML, route credentials, form tokens or attempt-local question IDs. Exam and
Chapter Work remain parse-only because this slice does not guess an Exam route,
start an assessment, add Capture behavior or claim live-account verification.

## One-hundred-and-twenty-first Phase 0 slice

Storage now defines immutable `QuestionSnapshot` records independently from
answer resolution and submission. Each snapshot is bound to an existing Task
and its actual Provider, contains one complete normalized Question set and is
written with all items in one SQLite transaction. Owner-scoped latest reads
join through the Task's ProviderAccount and revalidate persisted Question IDs,
positions, remote identities, Domain bounds and parent counts.

The schema caps each snapshot at 5,000 Questions and 16 MiB of serialized Domain
data. HTML, Provider route context, credentials, answers and submission payloads
have no storage columns. This slice exposes the repository boundary without
implicitly changing read orchestration; the following checkpoint connects its
explicit commit point.

## One-hundred-and-twenty-second Phase 0 slice

The Core Question read service now commits exactly one immutable
`QuestionSnapshot` after the complete Provider reference set and every parsed
Question pass owner, capability, assessment, identity, ordering, type, bounds
and sanitization checks. A duplicate reference, partial parse, binding drift or
storage failure returns no Question result and cannot leave a partial snapshot.

Fresh HTTP and CLI reads use the SQLite snapshot repository and return the
snapshot ID and capture timestamp beside the Provider version and normalized
Questions. The response remains `no-store` because it represents a fresh remote
read; persistence provides an explicit future binding point for AnswerCandidate
and SubmissionDraft, not permission to resolve answers or submit the Task.

## One-hundred-and-twenty-third Phase 0 slice

Provider-native `AnswerResolve` now has its own Provider trait and Registry slot
instead of being counted as an implementation of `TaskExecutionCapability`.
The contract accepts an already normalized Question set and can return bounded
Domain `AnswerCandidate` values, but it grants no draft-building, remote
execution or verification authority.

This slot is intentionally Provider-native. Manual input, local cache and
external answer banks remain Core-side `AnswerSource` concerns and must not be
forced through a learning-platform Provider or its credentials. Core binding,
all-or-nothing candidate validation and persistence remain later checkpoints;
Chaoxing advertises no answer-resolution capability in this slice.

## One-hundred-and-twenty-fourth Phase 0 slice

Storage now persists immutable multi-source `AnswerCandidate` records without
selecting a winner. Each record has a stable ID and a composite foreign-key
binding to one real Question inside one immutable `QuestionSnapshot`. Batches
may mix Manual, LocalCache, ProviderNative, ExternalBank and Other sources, but
a foreign Question, duplicate record, invalid normalized answer, unsanitized
provenance or database conflict rolls back the entire batch.

Owner-scoped reads join Candidate through QuestionSnapshot, Task and
ProviderAccount. Cumulative storage is capped at 20,000 Candidates and 16 MiB
per QuestionSnapshot, including across repeated batches. This is only the
candidate evidence boundary: it does not rank candidates, create a
SubmissionDraft, invoke a Provider execution slot or grant submission policy.

## One-hundred-and-twenty-fifth Phase 0 slice

Core now orchestrates Provider-native answer resolution against one explicitly
selected immutable `QuestionSnapshot`. Owner, Task, authenticated
ProviderAccount and Provider identity must all match; Core never silently swaps
in a newer snapshot. The Task must advertise `AnswerResolve`, and Registry must
provide the independent answer-resolution slot before Provider code is called.

Every returned Candidate must reference a Question in that snapshot, identify
its source as `ProviderNative`, satisfy Domain validation and be unique within
the bounded batch. Only the complete validated batch is persisted. Formal
assessment policy treats Resolve as read-only like Parse, while Execute and
Submit remain independently disabled. This checkpoint has no public API route
and Chaoxing still advertises no answer-resolution implementation.

## One-hundred-and-twenty-sixth Phase 0 slice

Provider-native resolution is exposed through an explicit Task plus
QuestionSnapshot HTTP path and `asterismctl task resolve-answers`. The route
requires Task Read permission, validates both typed IDs before orchestration,
returns `no-store`, and maps Provider rate-limit, availability, user-action,
remote-change and invalid-response failures without exposing raw details.

The response carries the immutable Snapshot binding, current Provider version
and persisted Candidate record IDs. This POST represents a read that records
new evidence; it deliberately does not use the Task execution route, accept an
answer payload, build a draft or grant any Submission capability. Providers
without `AnswerResolve`, including Chaoxing at this checkpoint, fail before a
Provider call.

## One-hundred-and-twenty-seventh Phase 0 slice

Persisted multi-source Candidates now have a separate owner-scoped HTTP/CLI
read surface. It resolves the exact QuestionSnapshot through Task and
ProviderAccount ownership, rechecks that the Task ID in the route matches the
snapshot, and only then loads Candidates in deterministic Question-position
and observation order. A foreign or mismatched snapshot is indistinguishable
from a missing one.

This GET performs no Provider call and remains distinct from Provider-native
resolution. It returns the same stable Candidate record shape and `no-store`
policy, providing an inspection boundary for later resolver/ranking work
without selecting answers or creating a SubmissionDraft.

## One-hundred-and-twenty-eighth Phase 0 slice

Core Domain now defines a bounded `SubmissionDraft` whose items retain the
normalized Question and one explicitly selected, persisted Candidate identity,
source, confidence and answer. Unknown answers, foreign Question bindings,
duplicate Candidates, invalid Questions and unbounded collections are rejected
before a draft can cross a boundary.

The Provider-specific payload preview is structured as an encoding, a bounded
format identifier and Question-bound field names. It deliberately cannot carry
an endpoint, headers, cookies, tokens, arbitrary JSON or executable field
values. The selected normalized answer remains the sole value source.

`SubmissionBuildCapability` is now an independent Registry slot and no longer
counts as `TaskExecutionCapability`. It may only describe a draft preview for
an explicit Question and selected-answer set; it grants no remote mutation or
verification authority. Chaoxing advertises no SubmissionBuild implementation
at this checkpoint, and draft persistence/orchestration remains a later slice.

## One-hundred-and-twenty-ninth Phase 0 slice

Storage now persists immutable `SubmissionDraft` records and reconstructs them
through owner-scoped reads. Database composite foreign keys enforce the full
Draft → QuestionSnapshot → Question → AnswerCandidate chain: a Candidate from
another snapshot or Question cannot be attached even when every UUID is known.

Before one transaction commits, Storage validates the complete Domain draft,
the snapshot Task/Provider binding, exact normalized Question content and the
selected Candidate's Question, answer, source and confidence. Any mismatch or
duplicate leaves no partial draft or item rows. Drafts are capped at 5,000
selected Questions and an 8 MiB structured preview, and their rows are never
updated through the repository contract.

Persisted items store only immutable identities and positions. Reads rebuild
the full reviewable draft from the authoritative Question and Candidate rows,
then validate it again; they do not persist or recover executable Provider
payload values. This checkpoint adds no public build endpoint and performs no
Provider call or remote mutation.

## One-hundred-and-thirtieth Phase 0 slice

Core now builds a `SubmissionDraft` only from an owner-scoped Task, one exact
immutable QuestionSnapshot and an explicit list of persisted AnswerCandidate
IDs. The Task and registered Provider must both advertise the independent
SubmissionBuild capability, and the ProviderAccount must still be authenticated
and bound to the same owner and Provider.

Selection is fail-closed: every Snapshot Question must have exactly one chosen
Candidate, while foreign, missing, repeated or multiple-per-Question identities
fail before the Provider is called. Provider code receives normalized Questions
and selected answers only and returns a non-executable payload preview. Core
validates the complete draft before its single repository commit.

Build is classified as a non-mutating formal-assessment action, independently
from Execute and Submit policy. The service creates no Execution, reserves no
credit, calls no Task execution slot and cannot submit a remote answer. Chaoxing
still advertises no SubmissionBuild implementation, and no public route is
added in this checkpoint.

## One-hundred-and-thirty-first Phase 0 slice

Submission draft construction is exposed as POST on an explicit
Task/QuestionSnapshot path. The request contains only persisted AnswerCandidate
IDs, requires Task Read permission and the request correlation header, and
returns the immutable draft with `201 Created` and `no-store`. The matching CLI
command requires Task ID, Snapshot ID and at least one Candidate ID.

A separate GET and CLI command read one persisted draft through the complete
Task/QuestionSnapshot/SubmissionDraft path. Ownership is resolved in Storage
and both route bindings are rechecked before any content is returned. Reading a
draft performs no Provider call; building one still uses only the independent
SubmissionBuild slot.

OpenAPI describes both operations and their bounded request identity list.
Neither route uses `/execute`, accepts raw answer or Provider payload values,
creates an Execution, reserves credit, or permits remote submission. A Provider
without SubmissionBuild, including Chaoxing at this checkpoint, returns a clear
capability conflict before draft creation.

## One-hundred-and-thirty-second Phase 0 slice

Core Domain now separates a bounded remote `SubmissionReceipt` from a
`SubmissionVerificationSnapshot` and final `SubmissionResult`. Receipts retain
only a status label, optional sanitized message, optional trace identity and
timestamp; raw response bodies, headers, request material and arbitrary JSON
are excluded.

Verification can record a confirmed, rejected, pending or inconclusive state,
bounded fixed-point score, progress, remote Task state and unique per-Question
facts. A Result marked confirmed or rejected must be supported by the matching
verification state. Receipt presence alone can never satisfy validation, and
all result, draft, Task, Snapshot, Provider and Question identities can be
validated together.

Provider Registry now gives SubmissionExecute and SubmissionVerify separate
slots from each other and from generic TaskExecutionCapability. Execute accepts
one validated immutable draft and frozen runtime settings and returns only a
receipt; Verify re-reads remote state and returns verification facts without
repeating the mutation. No Provider implements these slots yet, no execution
or result persistence is wired, and this checkpoint performs no remote write.

## One-hundred-and-thirty-third Phase 0 slice

Storage now persists immutable `SubmissionResult` records after validating the
bounded receipt, verification snapshot and status relationship. Database
composite foreign keys bind each Result to one real SubmissionDraft, Task,
Execution and ExecutionAttempt; one attempt cannot claim multiple results, and
an attempt from another Execution or Task cannot be attached.

The repository also verifies every per-Question result fact against Questions
selected by the bound draft. Foreign draft, snapshot, Provider, Question,
Execution or Attempt identities roll back the complete write. Receipt storage
is capped at 64 KiB and verification storage at 8 MiB, and neither boundary
permits raw request material or arbitrary response JSON.

Exact and latest-by-draft reads are owner-scoped through Draft → Snapshot →
Task → ProviderAccount, reconstruct the typed Result and validate it again.
This persistence contract does not create an Execution, invoke either Provider
submission slot or change Task state; the later worker integration must supply
the already active execution-attempt identity.

## One-hundred-and-thirty-fourth Phase 0 slice

Persisted Submission results now have an owner-scoped GET and CLI read surface
whose path carries Task, QuestionSnapshot, SubmissionDraft and SubmissionResult
identities. Storage resolves ownership first, and the API rechecks all three
parent bindings before returning the typed receipt and verification snapshot.

The response is `no-store` and performs no Provider call. A mismatched Draft or
Task path is indistinguishable from a missing Result even if the Result UUID is
valid. OpenAPI describes the complete route and makes the distinction between
receipt and verification visible to future generated clients.

This checkpoint intentionally exposes no Result creation endpoint and no
submission execution endpoint. Results can only originate from the internal
repository boundary that requires a real ExecutionAttempt; remote mutation and
formal-assessment submission policy remain unwired.

## One-hundred-and-thirty-fifth Phase 0 slice

Core now owns a Manual answer-candidate creation service rather than routing
manual evidence through a learning-platform Provider. Each command carries an
owner, Task, immutable QuestionSnapshot and Question identity. The service first
resolves the owner-scoped Snapshot, rechecks its Task path and requires the
Question to exist before constructing any candidate.

The caller supplies only the typed normalized answer, optional confidence and
optional explanation. Core fixes `AnswerSource` to `Manual` and creates a small
sanitized provenance marker itself, so callers cannot impersonate Provider,
cache or external-bank evidence and cannot inject arbitrary provenance JSON.
Unknown, malformed or unbounded answers fail before the repository write.

Creation persists one immutable candidate through the existing transactional
repository. It does not call a Provider, select a winning answer, build a
SubmissionDraft, create an Execution or submit anything remotely. HTTP and CLI
transport remain a separate checkpoint.

## One-hundred-and-thirty-sixth Phase 0 slice

The existing owner-scoped AnswerCandidate collection now accepts POST for one
Manual candidate while retaining GET for persisted multi-source evidence. The
request contains only an explicit Question identity, a typed NormalizedAnswer
and optional confidence/explanation. It cannot specify AnswerSource,
provenance, Provider payload material or a remote endpoint.

The HTTP handler uses Task Read authorization, parses every path and Question
identity, applies the 0-10,000 confidence bound and delegates all ownership and
snapshot binding to the Core Manual service. Success returns the immutable
candidate with `201` and `no-store`; malformed JSON or candidate data is a
stable bad request and hidden owner/binding mismatches remain not found.

`asterismctl task add-manual-answer` maps the same complete identity chain and
parses its `--answer` argument into the Domain NormalizedAnswer type before it
can send a request. This surface appends evidence only: it performs no Provider
call, candidate selection, draft build, Execution scheduling or remote submit.

## One-hundred-and-thirty-seventh Phase 0 slice

Core Domain now defines a bounded, reviewable `AnswerResolutionPlan` separately
from both candidate evidence and `SubmissionDraft`. Every Question decision is
explicitly `Selected`, `Conflict` or `Missing`, carries the considered Candidate
identities, and can only carry a selected Candidate and normalized answer in the
`Selected` state.

Plan validation caps Question and Candidate collections, rejects duplicate
identities, requires a selected Candidate to come from the considered set and
rejects `Unknown` selected answers. Conflict requires at least two known
Candidates and Missing requires none; neither unresolved state may conceal a
selection payload.

This is a transport- and persistence-independent contract only. It does not
define source preference, overwrite candidate evidence, persist a winner,
build a SubmissionDraft or authorize remote execution. The Core resolver that
constructs conservative decisions remains a separate checkpoint.

## One-hundred-and-thirty-eighth Phase 0 slice

Core now derives an `AnswerResolutionPlan` from the owner-scoped Candidate set
of one explicitly named immutable QuestionSnapshot. It rechecks the Task path,
every Question identity and position, every Candidate snapshot/Question binding
and all bounded candidate content before returning any decision.

The default policy is conservative and source-neutral. Unknown answers are not
eligible evidence. A Question is Selected only when every known normalized
answer is exactly equal; differing answers always become Conflict regardless of
source or confidence, and no known answer becomes Missing. Inside one consensus
group, the representative Candidate is chosen deterministically by highest
confidence, then newest observation and stable ID.

The resolver is read-only and does not call a Provider, mutate Candidate
evidence, persist a winner, build a SubmissionDraft or schedule an Execution.
Conflict and Missing decisions therefore remain explicit review requirements;
an HTTP/CLI inspection surface remains a separate checkpoint.

## One-hundred-and-thirty-ninth Phase 0 slice

The conservative resolver now has an owner-scoped GET surface whose path
contains both Task and immutable QuestionSnapshot identity. It uses Task Read
authorization, returns the typed `AnswerResolutionPlan` with `no-store`, and
maps a hidden owner or mismatched Task path to the same snapshot-not-found
response without calling a Provider.

OpenAPI exposes the read-only operation separately from Provider-native
candidate discovery and Manual candidate creation. `asterismctl task
answer-resolution <task-id> <snapshot-id>` maps the same exact binding so a
reviewer can inspect Selected, Conflict and Missing decisions before supplying
Candidate IDs to draft construction.

The endpoint performs no write and the returned plan is not submission
authority. It neither persists a winner nor implicitly builds or executes a
SubmissionDraft; every Conflict or Missing decision remains visible.

## One-hundred-and-fortieth Phase 0 slice

Chaoxing now implements `SubmissionBuild` for freshly confirmed pending
independent Work tasks. The capability revalidates the complete Question and
SelectedAnswer binding, permits only the supported Question/answer shapes, and
uses the primary donor's bounded `answer{qid}` / `answertype{qid}` form naming
to return a typed `chaoxing.work.form.v1` preview.

The preview remains structurally incapable of submission: it contains no answer
values, endpoint, `pyFlag`, Cookie, `enc`, token, header or arbitrary Provider
JSON. It performs no network request and grants neither `SubmissionExecute` nor
`SubmissionVerify`; unsupported complex question kinds fail closed.

Exam remains semantically separate from Work. The audited mobile flow reaches
attempt-local questions only after the mutating exam-start route yields an
active attempt and `enc`, potentially behind exam-code, face or captcha checks.
Asterism must implement that explicit start/attempt lifecycle and its
Capture/BrowserBridge or human-interaction gates rather than claim a fictitious
read-only path or borrow Work payload semantics. Mutation authorization,
receipt/verification separation and no ambiguous replay remain mandatory.

## One-hundred-and-forty-first Phase 0 slice

Core Domain now defines a versioned `QuestionContentFingerprint` for future
LocalCache evidence reuse. The SHA-256 material excludes snapshot-local
Question ID, Task ID, remote attempt QID and position, while retaining the exact
sanitized question kind, stem, option IDs/content/attachments, question
attachments and metadata.

This deliberately favors false negatives over unsafe matches: a changed option
ID or any normalized content change produces a different fingerprint. The
fingerprint is not sufficient authority by itself; storage and Core must still
scope matches to one owned Task and reject duplicate fingerprints on either
side before copying evidence. No candidate is queried, created or selected by
this Domain-only checkpoint.

## One-hundred-and-forty-second Phase 0 slice

Storage now persists `QuestionContentFingerprint` beside every newly written
Question snapshot item and verifies it against the decoded normalized Question
on reads. Migration 27 leaves legacy rows nullable instead of inventing hashes
for historical JSON; those rows safely remain ineligible until captured again.

The independent `AnswerCacheRepository` returns only direct, non-LocalCache
candidate evidence from earlier snapshots of the same owner-scoped Task. Its
join requires an exact indexed fingerprint and excludes the target or later
snapshot. Correlated counts require that fingerprint to occur exactly once in
both the target and each source snapshot; decoded source and target Questions
are then rehashed before any evidence crosses the storage boundary.

The query is bounded, deterministically ordered and returns no foreign-owner or
foreign-Task existence signal. This checkpoint does not create LocalCache
candidates: source evidence remains immutable, copied candidates cannot recurse,
and Core still owns the later import decision and provenance construction.

## One-hundred-and-forty-third Phase 0 slice

Core now imports eligible prior evidence into one explicitly named, owner-scoped
QuestionSnapshot as new `LocalCache` candidates. It revalidates the current
Task/Snapshot/Question set, every source Question/fingerprint/Candidate binding,
and rejects any evidence row that violates the storage ambiguity contract.

Imported candidates are rebound to the current Question ID. Core fixes their
source and bounded provenance, including the immutable source snapshot,
Question, direct Candidate, original source and content fingerprint identities;
callers cannot supply or impersonate these facts. Confidence is preserved but
the source explanation is replaced with a Core-owned cache marker.

Unknown answers, answer shapes incompatible with the current normalized
Question, and Matching/Composite/Unknown kinds are skipped. Repeating the same
import after its candidate is stored creates no duplicate. This remains an
evidence-only operation: it calls no Provider, does not select an answer, build
a draft, create an Execution or submit remotely. HTTP and CLI exposure remain a
separate checkpoint.

## One-hundred-and-forty-fourth Phase 0 slice

The LocalCache importer now has an owner-scoped POST surface at the explicit
Task/QuestionSnapshot path and a matching `asterismctl task
import-local-answers <task-id> <snapshot-id>` command. The request has no body,
source selector, fingerprint, answer or provenance input; those facts remain
derived by Storage and Core.

The response is `no-store` and returns only candidates newly imported by that
call. A repeat after successful persistence returns an empty candidate set with
200, while a hidden owner or mismatched Task path remains indistinguishable from
a missing snapshot. OpenAPI exposes this separately from Manual creation,
Provider-native resolution and the read-only candidate collection.

This transport performs no Provider call and does not chain into
AnswerResolution, SubmissionDraft construction, Execution scheduling or remote
submission. The caller must explicitly inspect the combined candidate set and
resolution plan after import.

## One-hundred-and-forty-fifth Phase 0 slice

The second first-batch Provider now starts with a clean-room WELearn research
baseline and fixture-only Rust crate. Exact donor revisions, ambiguous license
status, route/action evidence and drift risks are recorded before implementation;
no unlicensed donor source is copied into the repository.

Bounded parsers normalize `authCourse` Course rows and the `courseunits` →
`scoLeaves` hierarchy. Numeric and string remote IDs share one stable form; the
stable `cid` remains Course identity while its duplicate route-context copy is
omitted from serialization. Unknown fields are dropped, and duplicate or
misbound Course/Unit/SCO identities fail closed. SCO completion,
visibility and opaque donor `learntime` are retained as independent facts:
unknown completion stays `Unknown`, a hidden Unit forces `NotOpen`, and duration
alone never implies completion.

The new metadata remains `Development` with no advertised runtime slots,
authentication methods or session kinds. This historical checkpoint had not yet
implemented HTTP, authentication, CMI mutation, execution, duration reporting
or Capture; every audited donor ability remained an active implementation gap,
and the checkpoint was not evidence of current live WELearn compatibility.

## One-hundred-and-forty-sixth Phase 0 slice

WELearn now has the common Password/ImportedCookie authentication orchestration
behind injected exchange, Cookie-validation and stored-session resolver
boundaries. Password candidates must contain exactly username/password fields
acquired through NativeProviderLogin; successful exchange produces a Composite
replacement containing the renewable credentials and validated Cookie. Imported
Cookies are validated without a password exchange, and stored-session checks
require an explicit credential reference.

The current SSO password form encoding is deterministic around one explicit
millisecond timestamp. Bounded synthetic envelopes distinguish ordinary
credential rejection, ImageCaptcha and SmsVerification; challenges stop at
`HumanRequired` rather than retrying. Successful responses must yield an HTTPS
callback under exactly `sso.sflep.com/idsvr/`, while callback query values,
Cookie contents and verification tokens are redacted and zeroized.

Metadata remains `Development` and advertises only this Authentication boundary.
The native prelogin/OIDC redirect chain, Cookie collection, authenticated
validation request, renewal and Provider registry entry remain separate; this
checkpoint performs no remote request and makes no live-compatibility claim.

## One-hundred-and-forty-seventh Phase 0 slice

The WELearn Course and Task parsers now sit behind authenticated
`CourseInventoryCapability` and Course-scoped `TaskInventoryCapability`
adapters. Context/provider mismatches, absent credential references and missing
Course scope fail before transport. Response documents are bounded, redacted
from diagnostics and zeroized when dropped.

Task inventory is all-or-nothing across the Unit hierarchy: the transport must
return one `scoLeaves` response for every Unit, including an explicit empty
response when no leaves exist. Missing, duplicate or out-of-range Unit responses
and duplicate Course/SCO identity reject the whole scan, so Core cannot persist
a partial task set after an interrupted transport.

Development metadata now advertises exactly Authentication, CourseInventory and
TaskInventory, and a registry-consistent Provider entry can be composed from
their injected boundaries. No TaskDetail, Progress, Duration, Execution,
BrowserBridge or runtime setting is claimed. Native HTTP, stored credential
resolution and daemon registration remain later checkpoints.

## One-hundred-and-forty-eighth Phase 0 slice

WELearn Course and Task inventory now have a native reqwest transport built only
from the shared resolved NetworkProfile. Redirects remain disabled. Fixed HTTPS
routes cover `authCourse`, the fresh Course page, `courseunits` and every indexed
`scoLeaves` read; dynamic query values are URL-encoded rather than interpolated.

One Task scan resolves its account Cookie once, reacquires `uid` and `classid`
from the selected Course page, and uses that same session for Unit and SCO reads.
Course context accepts repeated identical facts but rejects missing, conflicting
or unsafe values, remains redacted in diagnostics and is never serialized.
Response status, bounded Retry-After, login redirects/pages, content type,
content length, UTF-8 and streaming body size are classified before parsing.

The native inventory transport can compose a registry-consistent Development
entry with an injected authentication exchange. Password SSO/OIDC HTTP, stored
credential resolution, renewal and daemon registration are still absent, and
no real WELearn account was contacted by this checkpoint.

## One-hundred-and-forty-ninth Phase 0 slice

WELearn Password authentication now has a native transport around the shared
non-redirecting NetworkProfile client. The prelogin and OIDC flows are followed
manually for at most twelve hops, and every requested or returned URL must stay
on HTTPS at exactly `welearn.sflep.com` or `sso.sflep.com`. The decoded
`returnUrl` is unique, bounded and restricted to the expected authorize callback
shape; the login callback retains the existing stricter SSO route check.

The transport collects Set-Cookie fields in a bounded redacted jar. Cookie
domains are limited to the audited SFLEP scopes, domain and path rules are
applied for every hop, replacements zero the prior value, and empty or
`Max-Age=0` deletions remove retained values. Response bodies, redirect state and Cookie
values are excluded from diagnostics; owned login buffers and retained secret
values are zeroized.

A completed exchange returns a bounded Cookie candidate, but Authentication
accepts it only after a native Course-list read parses successfully. A fully
native Development entry can now be composed around an injected stored-session
resolver. Persistent credential resolution, renewal, daemon registration and
live-account validation remain later gates, so this checkpoint does not raise
the Provider verification level.

## One-hundred-and-fiftieth Phase 0 slice

WELearn can now compose its native Authentication and inventory transports
around Core's `ProviderCredentialResolver`. The Provider requests only the
Cookie purpose using the exact account ID, opaque credential references and
correlation ID from `ProviderContext`; it never reads persistence directly.

Resolution fails closed unless Core returns exactly one credential belonging to
the requested account and one of the requested secret IDs. Its metadata must
name the Cookie purpose, use Cookie or Composite session kind and remain
unexpired. Invalid UTF-8, malformed Cookie fields, duplicates, unrelated
credentials and authorization/not-found failures are reported as
Authentication failures, while key/storage failures remain sanitized Internal
errors. Plaintext is owned by Core's zeroizing resolved credential and copied
only into the bounded redacted Cookie session used by the request.

The resulting Development entry is registry-consistent and its current native
path has no Capture dependency. Automatic password renewal, daemon opt-in
registration, Capture/browser compatibility and live-account validation remain
separate active gates rather than Provider completion exclusions.

## One-hundred-and-fifty-first Phase 0 slice

The daemon can now register the complete WELearn Development entry around a
provider-scoped `SqliteProviderCredentialResolver`. Registration is disabled by
default and requires the independent `enable_development_welearn` configuration,
`ASTERISM_ENABLE_DEVELOPMENT_WELEARN` environment variable or
`--enable-development-welearn` CLI flag. Enabling any development Provider also
requires the SecretStore keyring before startup can continue.

Chaoxing and WELearn opt-ins are evaluated independently, so either or both can
be registered without one platform's disabled state short-circuiting the other.
Each gets its own canonical Provider ID and resolver scope. Startup emits an
explicit unverified-development warning, while metadata remains Development and
Capture stays disabled.

This makes the native WELearn authentication and Course/Task scan path reachable
for intentional local validation. It is not a live verification result:
automatic renewal is still absent, expired sessions fail authentication, and no
verification level is raised.

## One-hundred-and-fifty-second Phase 0 slice

WELearn stored Composite sessions can now renew through Core's scoped resolver
and compare-and-replace boundary. Renewal requires exactly one username,
password and Cookie from the requested account and secret-reference set; every
credential must be unexpired, use Composite session kind and originate from
NativeProviderLogin. Duplicate IDs/purposes, incomplete bundles, imported
Cookie-only sessions and stale metadata fail before login.

The bounded Password/OIDC exchange validates its fresh Cookie with an
authenticated Course-list read before Core atomically replaces the complete
credential set. Renewal is serialized across a fixed per-account lock pool and
cached only for the same account, credential references and operation
correlation, with both TTL and entry bounds. Plaintext inputs, old/new Cookies
and login bodies retain the existing redaction and zeroization boundaries.

Authentication, Course inventory and Task inventory retry only an explicit
Authentication failure, at most once. Task inventory restarts the complete
Course-page → Unit → SCO sequence after renewal, preserving its all-or-nothing
contract. A bounded `200 text/html` response is inspected for structural login
form markers before Content-Type rejection, while unrelated HTML remains an
InvalidResponse and cannot trigger credential use. The daemon now composes this
renewing resolver by default whenever its WELearn Development opt-in is enabled.
This remains offline/native-boundary evidence, not live verification.

## One-hundred-and-fifty-third Phase 0 slice

WELearn now exposes read-only `TaskProgressRead` for every normalized SCO. The
native transport refreshes the selected Course page for its current account
route, then posts only `getscoinfo_v7` with the stable Course and SCO identities.
It shares the scoped Cookie resolver and one Authentication-only renewal retry;
no missing or malformed read can fall back to starting or mutating a SCO.

The bounded redacted CMI document parser handles the audited outer JSON
`comment` envelope and its nested JSON independently. Completion status and
decimal progress map to `RemoteProgress`, while raw `session_time` and
`total_time` remain separate parser facts. Because neither the current static
donors nor a live sanitized fixture establishes one reliable time unit and
grammar, `duration_seconds` is deliberately unset.

Metadata and the registry-consistent Development entry now advertise and wire
`TaskProgressRead`; this historical checkpoint had not yet implemented
execution, heartbeat, finalize, duration reporting or Capture, which remained
active audited gaps. Synthetic fixtures and negative tests cover identity
binding, bounded nested parsing, independent facts and fail-closed drift. This
checkpoint is offline/native-boundary evidence only and does not raise WELearn's
verification level.

## One-hundred-and-fifty-fourth Phase 0 slice

The third first-batch Provider now starts with a frozen clean-room UAI audit and
a parser-only workspace crate. Three exact donor revisions separate current
Password/JWT and backend Course/progress behavior, independent duration-summary
evidence, and page-residence compatibility behavior. Apache-2.0, MIT and
GPL-3.0 boundaries are recorded explicitly; the GPL userscript remains
Reference-only and no implementation code is copied.

The bounded Course parser flattens each nested CourseResource into one stable
`RemoteCourse`, retains only paired point counts, and excludes class/current
instance routes. A fresh resource-detail parser must bind the repeated resource
ID before holding `courseInstanceId` in a redacted operation context. The Task
parser then decodes the outer `course` JSON string, bounds its nested tree and
emits stable Group Tasks with Unit, Section and Node/Micro ancestry kept as
separate normalized facts.

Synthetic fixtures and negative tests cover numeric/string resource IDs,
duplicate identities, impossible point totals, misbound details/contexts,
unknown roles and route-data exclusion. At this historical parser checkpoint,
metadata advertised no runtime capability; authentication, native transport,
progress, completion, duration, execution, submission, BrowserBridge and
Capture were all still active implementation gaps. This is fixture-only evidence
and not live UAI compatibility.

## One-hundred-and-fifty-fifth Phase 0 slice

UAI now exposes injected Password and ImportedToken Authentication without a
Capture dependency. Password candidates require exact username/password fields
and produce username, password and one atomic ProviderCompositeSession only
after the transport validates the returned session. Manual imports accept one
strict JSON document containing both openid and a three-segment JWT; unknown
fields, unsafe route identities and malformed tokens fail before transport use.

The bounded login classifier accepts only the donor-observed string success
code and complete result pair. Ordinary failures remain Authentication errors,
while code `1506` becomes an image/slider `HumanRequired` state. Response
messages, open IDs, JWTs and serialized composite values remain redacted and
zeroized across their owning boundaries.

An injected account-bound resolver also supports stored-session validation; at
this historical checkpoint native HTTP, Core persistence wiring, JWT
expiry/recovery and automatic Password renewal were not yet implemented and
remained required work. Metadata therefore adds only Authentication plus
Password/ImportedToken and Jwt/Composite session declarations; Course/Task
parsers remain offline helpers rather than advertised registry slots. This is
offline orchestration evidence, not live authentication.

## One-hundred-and-fifty-sixth Phase 0 slice

The UAI stored-session boundary now uses Core's provider-scoped credential
resolver rather than an abstract test-only source. Each operation requests only
the referenced ProviderCompositeSession purpose, then verifies exact account,
secret-reference, purpose, expiry and lifecycle metadata before parsing any
plaintext.

Two origins are accepted deliberately: ManualImport paired with Jwt metadata,
or NativeProviderLogin paired with Composite metadata. Wrong origin/kind pairs,
duplicates, unrelated secret IDs, foreign accounts, expired values and
malformed composite JSON all fail as Authentication. Storage, key and
ciphertext-authentication failures remain sanitized Internal errors.

The resolver exposes only the bounded redacted openid/JWT session needed by the
injected validation transport. At this checkpoint it did not resolve
username/password, perform renewal or implement native HTTP; those remained
required subsequent work. This closes the Core resolution side of session
validation without claiming live validity, expiry recovery or native login
compatibility.

## One-hundred-and-fifty-seventh Phase 0 slice

UAI now has a concrete native Authentication transport built from the shared
resolved network policy and its non-redirecting HTTPS client. Password exchange
posts the audited JSON fields to the fixed SSO route, classifies HTTP status and
JSON media type before consuming a bounded UTF-8 body, then reuses the typed
success/rejection/slider classifier. The returned JWT is placed directly in a
sensitive `Authorization` header, without an inferred Bearer prefix.

Session validation performs only the audited user-info read and requires both
bounded `appUserId` and `ssoId` identity markers without retaining the profile
document. Login and user-info responses share a 64 KiB limit; rate limits,
authentication rejection, route drift, provider outages and malformed bodies
remain distinct sanitized errors, while body buffers are zeroized after use.

This makes Password exchange and stored/imported JWT validation concrete at the
native HTTP boundary, but does not register UAI in the daemon or claim live
compatibility. Password renewal, native Course/Task/progress reads, duration,
execution, BrowserBridge and Capture remain separate slices.

## One-hundred-and-fifty-eighth Phase 0 slice

UAI now advertises and implements read-only `CourseInventory`. The capability
rejects mismatched Provider or credential-free contexts before transport use,
then accepts exactly one complete redacted response document and runs the
existing bounded Course → CourseResource parser. Each resource remains a
stable Course identity with point counts kept independent from completion.

The native transport resolves only the operation account's atomic openid/JWT
session, sends the raw JWT as a sensitive Authorization value to the fixed
Course-list route, and uses the shared non-redirecting HTTPS policy. HTTP
status, JSON media type, Content-Length, streamed 4 MiB limit and UTF-8 are
validated before parsing; raw bodies are zeroized and cannot produce partial
inventory.

This checkpoint has offline capability and native-boundary coverage only. It
does not register UAI in the daemon, renew Password sessions, fetch fresh
resource detail/tree data, read progress or add execution, duration,
BrowserBridge or Capture behavior.

## One-hundred-and-fifty-ninth Phase 0 slice

UAI now advertises and implements read-only `TaskInventory` as one complete
fresh-detail/tree boundary. Every request requires an explicit normalized
Course. The native transport derives only its stable CourseResource ID, reads
the resource detail, and must bind the repeated ID before using the returned
`courseInstanceId` for a tree request.

The fresh instance is encoded as one URL path segment and retained only in the
redacted operation context. Both authenticated JSON responses share the
status/media-type/UTF-8/stream-size safeguards and are returned together or not
at all. The capability revalidates the detail binding before decoding the
outer `course` JSON and emitting stable Group Tasks with separate Unit, Section
and Node/Micro ancestry.

This remains offline/native-boundary evidence and UAI is not daemon-registered.
No session renewal, progress read, duration, execution, BrowserBridge or
Capture behavior is added by this checkpoint.

## One-hundred-and-sixtieth Phase 0 slice

UAI now advertises native read-only `TaskProgressRead`. Group Task identities
include stable CourseResource, Unit and Group components so progress routing
does not guess ancestry or persist `courseInstanceId`. Each read resolves the
account-bound openid/JWT, rebinds a fresh resource detail, then fetches the
exact per-Unit progress document.

Two frozen backend donors independently corroborate the HS256
`x-annotator-auth-token` contract used beside the raw JWT. Asterism generates
that token only inside this account-bound GET transport, marks both headers
sensitive and pins the millisecond-expiry claims/signature with an exact-time
test vector. Route segments, response status/media type/UTF-8 and a 1 MiB body
limit all fail closed.

The parser requires the repeated Unit and exact Group leaf. Only
`pass == pass2 == perm == 1` maps to Completed and 100%; other valid flag sets
remain Unknown without a percentage. Per-Group numeric duration is retained as
an untyped raw fact and never mapped to `duration_seconds`. This remains
offline/native-boundary evidence: daemon registration, session renewal,
DurationRead/Report, execution, BrowserBridge, Capture and live verification
are still absent.

## One-hundred-and-sixty-first Phase 0 slice

UAI native sessions can now renew through Core's provider-scoped resolver and
compare-and-replace boundary. Renewal requires exactly one username, password
and ProviderCompositeSession from the requested account/reference set; all
three must be unexpired Composite credentials originating from
NativeProviderLogin. ManualImport+Jwt, mixed lifecycle metadata, duplicates and
incomplete sets fail before login.

The Password exchange must validate its fresh JWT through user-info before Core
atomically replaces the complete three-field bundle. Renewal is serialized
through a fixed per-account lock pool and cached only for the same account,
credential references and correlation, with TTL and entry bounds. Debug output
and plaintext lifetimes retain the existing redaction and zeroization rules.

Authentication, CourseInventory, TaskInventory and TaskProgressRead retry only
an explicit Authentication failure and at most once; multi-request Task and
progress operations restart from fresh detail after renewal. This is still
offline/native-boundary evidence. Daemon registration, JWT expiry extraction,
DurationRead/Report, execution, BrowserBridge, Capture and live verification
remain separate.

## One-hundred-and-sixty-second Phase 0 slice

UAI now has one registry-consistent Development factory for Authentication,
CourseInventory, TaskInventory and TaskProgressRead. The native composition
shares one resolved NetworkProfile, one account-scoped stored-session resolver
and one inventory transport; the renewal variant binds Core's credential
resolver and atomic renewer to the native Password/user-info transport.

The daemon can register that complete entry through the independent
`enable_development_uai` configuration, `ASTERISM_ENABLE_DEVELOPMENT_UAI`
environment variable or `--enable-development-uai` CLI flag. Registration is
false by default, requires the configured SecretStore keyring and emits the
same unverified Development warning as the other first-batch Providers.

Factory and daemon tests require the metadata and populated registry slots to
agree and prove UAI can be enabled independently. No runtime settings,
DurationRead/Report, execution, BrowserBridge or Capture slot is inferred, and
this composition checkpoint does not claim live compatibility.

## One-hundred-and-sixty-third Phase 0 slice

`DurationRead` now has its own Provider registry slot and typed
`RemoteDuration` result instead of borrowing the mutation-oriented
TaskExecution capability. `DurationReport` remains on the execution boundary;
advertising or implementing either one never implies the other.

Core exposes one owner-scoped read service and `GET
/api/v1/tasks/{task_id}/duration`, mirrored by `asterismctl task duration`.
The service requires the persisted Task to advertise DurationRead, verifies
owner/account/authentication binding, passes only opaque credential references
and a bounded correlation ID, and never creates an Execution or invokes a
progress/reporting slot.

Registry, Engine, API/OpenAPI and CLI tests prove the capability stays separate
from progress and remote mutation. Providers still need their own normalized
seconds evidence before populating this slot.

## One-hundred-and-sixty-fourth Phase 0 slice

UAI now populates the independent `DurationRead` slot from its native
study-record boundary. The frozen MIT donor explicitly documents Course, Unit
and nested Task duration values in seconds and passes the numeric
CourseResource ID—not strategy ID—as query parameter `id`.

Each read parses `group:{courseResource}:{unit}:{group}`, resolves or atomically
renews the account-bound JWT, fetches only bounded `appUserId` and `ssoId`
identity facts, then reads `unitTaskSituation` for that exact CourseResource
and Unit. Authentication retry restarts the complete user-info/duration pair at
most once. Returned documents are bounded, redacted and zeroized.

The parser requires one matching Unit, globally unique bounded node identities
inside it, audited hierarchy roles and one exact matching Group identity with
an explicit unsigned duration. Duplicate/missing identities, unknown roles and
missing, negative or overflowing values fail closed. Normalized UAI Group Tasks
now persist both ProgressRead and DurationRead. The separate signed progress
route still retains its numeric duration only as an untyped raw fact.

The registry-consistent Development factory now composes all five read-only
capabilities, while daemon opt-in and verification level remain unchanged.
This is offline/native-boundary evidence only. DurationReport, execution,
BrowserBridge and Capture remain active audited gaps; real-account read-only
validation is scheduled after WebUI and Asterism-Plugin/Yunzai are complete.

## One-hundred-and-sixty-fifth Phase 0 slice

UAI now advertises a fresh `TaskDetail` capability instead of returning the
persisted scan payload as current state. It parses the stable
`group:{courseResource}:{unit}:{group}` identity, re-lists CourseResources,
selects exactly one match, re-runs the complete fresh detail/tree inventory and
requires exactly one matching Group. Disappeared CourseResources or Groups are
RemoteChanged; duplicate and normalized-identity drift fail closed.

The bounded tree parser now preserves optional `question_num` beside the
existing `base` task types and emits `v1:`-prefixed normalized fingerprints.
These facts are visible in the sanitized fresh detail but do not authorize
question access or submission by themselves. The Development factory and
metadata now agree on six read-only registry slots.

The frozen backend audit also records separate encrypted content, encrypted
answer, submit and fresh user-module verification routes. They map to
QuestionInventory/QuestionParse, AnswerResolve, SubmissionBuild,
SubmissionExecute and SubmissionVerify rather than one monolithic execute
operation. Rate-limit responses are not receipts, a receipt is not
verification, and unsupported empty/upload/discussion behavior cannot be
invented as a default. Those five capabilities remain unadvertised pending
their own bounded offline slices; live-account validation timing is unchanged.

## One-hundred-and-sixty-sixth Phase 0 slice

UAI now fills the independent `QuestionInventory` and `QuestionParse` slots
without reading the encrypted standard-answer route or granting any submission
authority. Fresh TaskDetail must still identify exactly one
`group:{courseResource}:{unit}:{group}` and explicitly advertise both Question
capabilities. Only donor-shaped `single-choice`, `multichoice` and
`short_answer` Groups with a positive bounded `question_num` currently opt in;
all other task types fail closed.

The native transport re-reads CourseResource detail, keeps the resulting
`courseInstanceId` operation-local and obtains exactly one bounded v3 content
response with raw sensitive JWT Authorization and a sensitive annotator token.
An Authentication failure restarts the complete detail/content pair at most
once. Redirects, non-JSON media, oversized bodies and identity drift remain
typed failures.

The answer-free parser requires the `unipus.` hexadecimal framing, a bounded
eight-byte key suffix joined to the audited `1a2b3c4d` prefix, AES-128 ECB,
valid PKCS#7 or donor-observed zero padding, a bounded decrypted array and one
bounded nested content JSON document per fresh question count. It retains only
attempt-local identity, normalized visible stem text and visible options;
ciphertext, key suffix, unknown fields, answer-shaped fields and route material
never enter the Domain Question. Inventory and parsing share a five-minute,
account/correlation/task-scoped in-memory attempt cache that is consumed after
the last Question.

Metadata and the Development factory now agree on eight read-only capability
slots. The fixture is synthetic encrypted data, verification remains
Development, and real-account read-only validation remains scheduled after the
WebUI and Asterism-Plugin/Yunzai surfaces are complete. `AnswerResolve`, every
submission stage, BrowserBridge and Capture remain active audited gaps rather
than a later-phase exclusion.

## One-hundred-and-sixty-seventh Phase 0 slice

WELearn now advertises fresh `TaskDetail` beside its existing authenticated
Course/Unit/SCO inventory and read-only CMI progress. Detail parses only the
stable `sco:{course}:{sco}` identity, re-lists current Courses, selects exactly
one match, then re-runs that Course's complete all-or-nothing Unit/SCO scan and
requires exactly one matching SCO. It never trusts persisted `uid`, `classid`
or Unit routing values.

Disappeared Course or SCO identities are RemoteChanged; malformed, duplicate
or normalized-identity drift fails closed. WELearn Task fingerprints now use
the Core-compatible `v1:{sha256}` grammar, and the detail payload retains only
the already-sanitized normalized SCO observation.

Metadata and the Development factory now agree on five read-only/authenticated
slots. This remains synthetic/offline evidence at Development verification;
CMI times remain opaque, while DurationRead/DurationReport, donor mutation and
remote execution remain active audited gaps. Real-account read-only validation
is scheduled after the WebUI and Asterism-Plugin/Yunzai surfaces are complete.

## One-hundred-and-sixty-eighth Phase 0 slice

WELearn now implements `DurationReport` as an execution goal independent of
completion-changing `ResourceExecution`. Master-owned runtime settings expose
bounded platform defaults with account/task overrides for actual report length
and heartbeat interval, while Provider/account execution concurrency and scan
interval remain separate settings.

The native lifecycle resolves one current Cookie and fresh Course route, reads
CMI, starts only when a valid response explicitly has no CMI, waits in real
time while sending the audited heartbeat action, finalizes with the original
completion/progress/score/success values and performs a fresh readback. A
post-start baseline and final readback must contain the complete preservation
set. Missing fields are never replaced with synthetic defaults; success also
requires at least one opaque raw time observation to change without any
preserved state drift. Mutation responses use the strict audited integer result
grammar, and no mutation is renewed or replayed after failure.

The shared Engine now accepts verified Pending, InProgress or Completed state
for an only-DurationReport request because the goal is duration evidence, not
remote completion. Every Provider error goes directly to HumanRequired without
retry, and abandoned duration-report recovery neither reads TaskProgress nor
re-enters start/keep/save. Provider and Engine regression tests cover these
boundaries. `DurationRead` remains unadvertised because live evidence has not
established the unit/grammar of WELearn's raw time values. Completion/progress/
score mutation, assessment/exercise execution and Capture remain separate active
capability work; preserve-only reporting is not the only permitted write.
Verification stays Development.

## One-hundred-and-sixty-ninth Phase 0 slice

Independent submission execution is now a first-class Core worker path rather
than falling through generic `TaskExecutionCapability`. Scheduling requires one
owner-scoped immutable `SubmissionDraft`, freezes its identity on the
`Execution`, checks Task, Provider and implementation-version bindings, and
permits a Draft to be consumed by at most one Execution. Idempotent replay must
match that same Draft; non-submission Tasks reject one.

The worker invokes `SubmissionExecute` at most once for the active Attempt,
validates and durably persists its bounded receipt, then invokes the separate
`SubmissionVerify` slot. A receipt never closes the Execution. Only a valid
Confirmed verification with Completed remote state persists a Confirmed result
and succeeds; Rejected persists its result and fails, while Pending,
Inconclusive, protocol drift and mutation-side transport ambiguity atomically
move the existing Attempt into Recovering.

Submission recovery reloads the same frozen Draft and active Attempt, supplies
the persisted receipt when available, and calls only `SubmissionVerify`.
Pending and transient verification failures may defer another read, but never
create a Retry job or invoke Submit again. A crash before receipt persistence
therefore verifies with no receipt rather than guessing that the mutation did
not occur. Existing terminal results are reused after a crash between result
persistence and Execution finalization. Storage and Engine regressions cover
Draft uniqueness, receipt idempotency, confirmed execution, receipt-with-
Pending recovery and ambiguous submit recovery without resubmission.

The target-aware DurationReport contract is aligned at scheduling as well as
execution: an only-DurationReport goal may run for Pending, InProgress,
Completed or explicitly Unknown completion state, because a verified duration
change must not invent or require a completion value. Any ambiguous duration
mutation still goes directly to HumanRequired and cannot be automatically
retried or recovered through TaskProgress.

## One-hundred-and-seventieth Phase 0 slice

Chaoxing independent Work now composes the first Provider-specific consumer of
Core's separate submission slots. A freshly confirmed pending Work advertises
Build, Execute and Verify together, while Exam and Chapter Work remain outside
this path. Build stays value-free; the executable subset is deliberately
limited to single-choice, multiple-choice and true/false answers whose option
and Provider type codes have an unambiguous donor encoding.

Execute revalidates the immutable Draft and expected preview, rediscovers the
current Course and Work route, and requires a fresh allowlisted editor with the
same complete Question/type set. It forwards only a fixed bounded set of hidden
form fields, drops unknown and stale answer values, adds the Draft-derived
answers and final-submit flag, and calls `addStudentWorkNew` exactly once. The
bounded success JSON becomes only an accepted Receipt. Session renewal may
repeat a known-failed editor GET but never a mutation with an ambiguous result;
challenge, expiry, authentication and protocol branches remain typed.

Verify independently rediscovers the same Work and can run with no Receipt after
a crash or ambiguous POST. Editor and submission-prompt pages remain Pending.
Only an allowlisted result view with a complete unique QID/type set and an exact
server-visible answer match for every Draft item returns Confirmed/Completed;
mismatches are Rejected and incomplete facts are Inconclusive. Synthetic
fixtures and capability tests cover form allowlisting, acknowledgement-only
semantics, independent slots, no-Receipt recovery and exact answer readback.
No live compatibility or verification-level promotion is claimed. Capture and
BrowserBridge compatibility remain active work outside this native Work slice.

## One-hundred-and-seventy-first Phase 0 slice

The management API now reads an exact persisted Question snapshot through its
full owner-scoped Task/Snapshot identity chain without contacting the Provider.
The response reuses the same bounded sanitized shape as a fresh Question read,
while mismatched Task IDs and cross-owner snapshots remain indistinguishable
from missing records. OpenAPI and generated-client contract checks require the
typed response.

The first formal WebUI answer workflow consumes this immutable read after page
reloads. Provider-native, local-cache and manual candidates remain separate
evidence; conservative resolution preselects only unambiguous decisions, every
conflict or missing answer requires an explicit selection, and Draft build
still passes exact persisted Candidate IDs through the shared Core action.
Execution binds the returned immutable Draft and a stable idempotency key.
Formal assessments remain blocked because a durable approval contract does not
yet exist; the browser cannot turn a local confirmation into execution policy.

## One-hundred-and-seventy-second Phase 0 slice

Task approval, cancellation, delay and ignore now use one owner-scoped Engine
service and four versioned HTTP Core Actions shared by WebUI, CLI and future
integration clients. Every action requires a caller-stable idempotency key and
persists an exact receipt; replay returns the original outcome, while reusing a
key for another Task, action or delay timestamp conflicts before mutable state
is inspected.

Approval is deliberately narrow: only WaitingApproval moves to Ready. It does
not set or persist any formal-assessment execution/submission permission, so a
formal Task remains blocked by the independent assessment guard. Execute can no
longer schedule directly from WaitingApproval. Ignore changes only local
orchestration and never contacts a Provider.

Delay is accepted only for a future timestamp while the Task and its single
active Execution are Scheduled and the initial scheduler Job is still Pending;
the Execution and Job timestamps move in one immediate transaction. Cancel can
close a Task with no Execution, or atomically cancel a non-running Requested,
Scheduled, RetryWaiting or HumanRequired Execution, cancel all matching pending
Jobs, release an active credit reservation, write sanitized Audit, persist the
receipt and enqueue Task/Execution/Credit outbox events. A claimed Job, lease,
Running or Recovering mutation conflicts instead of presenting a false local
cancellation while remote work may continue.

The generated OpenAPI client exposes strong response types for all four
actions. The Task page renders state-aware approve, cancel, delay and ignore
controls, preserves idempotency keys across retries, and clearly separates
orchestration approval from the formal-assessment safety barrier. Storage,
Engine and HTTP regressions cover replay, waiting-state bypass prevention,
atomic delay, atomic cancellation/refund and claimed-job refusal.

## One-hundred-and-seventy-third Phase 0 slice

The shared administration boundary now exposes password-free `UserProfile`
records rather than serializing the authentication `User` model. Master Web
Sessions can page, create, read and revision-update users through versioned
routes; username uniqueness, Argon2id hashing, roles, explicit permissions and
the zero-balance credit account are committed atomically. Updates compare the
client's exact `updated_at` revision and refuse to suspend, disable or demote
the final active Master.

Every accepted user mutation writes one sanitized Audit record and a
`UserChanged` outbox event in the same transaction. Audit queries use an
explicit authority: `ViewAnyAudit` sees the global log, while ordinary
`ViewOwnAudit` Web identities and owner-bound `AuditRead` service identities
see only their user actor, owned service-token actors and resources targeting
that owner. Authentication hashes, plaintext passwords and unsanitized
metadata never enter an administration or audit response.

Service-token management now has a paginated metadata-only list route. A
Master Web Session can manage the complete set; a delegated owner-bound token
with `ServiceTokenManage` can list, derive and revoke only tokens owned by the
same user. Foreign token IDs are hidden as not found and checked before the
revocation transaction, closing the previous cross-owner ID authorization
gap. OpenAPI declares strong page/profile/audit schemas, and storage plus HTTP
regressions prove optimistic concurrency, final-Master protection, password
redaction, Audit scoping, token-scope non-escalation and cross-owner isolation.

## One-hundred-and-seventy-fourth Phase 0 slice

The first-batch Provider completion boundary is now the complete set of
capabilities supported by audited donors, PortSources and reliable References,
not a read-only, non-Capture or Native-HTTP-only subset. Completion, progress
and score mutation, direct task execution, answer submission, Capture,
BrowserBridge, browser storage/runtime state, dynamic cryptographic context and
local authentication assistance are all in scope when upstream evidence exists.

Native Rust HTTP remains the first engineering choice, but an incomplete native
path must continue into the smallest Provider-specific BrowserBridge or Capture
fallback that preserves the donor behavior. A milestone only reports progress.
A Provider stops only after every audited ability is implemented and receives
all currently executable verification, every remainder has a concrete hard
blocker that further Capture/BrowserBridge/Core/donor work cannot resolve, or
Main explicitly stops it.

This scope expansion does not weaken the implementation contract. Bounded I/O,
fresh identity rediscovery, immutable Drafts, receipt/verification separation,
no ambiguous replay of non-idempotent mutations, zeroizing secrets, typed
errors, protocol-drift fail-closed behavior, owner/account/task binding,
durable recovery and separate Capability semantics remain mandatory. Live
mutation validation still requires authority matching its remote effect, while
code, fixtures and native/browser boundaries continue before that gate.

## One-hundred-and-seventy-fifth Phase 0 slice

Task execution now freezes the caller's exact, canonical executable Capability
selection on the durable `Execution`. HTTP, OpenAPI, CLI and WebUI require an
explicit non-empty selection; persistence rejects duplicates, read-only
capabilities, submission/action mixtures and idempotency replay with a changed
selection. The scheduler, Provider request and recovery path use that frozen
selection rather than re-deriving every action advertised by a later Task
snapshot.

`ExecutionVerify` is now action-scoped. The Task marker advertises that at
least one action supports goal-bound verification, while
`TaskExecutionCapability::requires_execution_verification` decides for the
frozen selection. This lets one WELearn SCO expose independent
`ResourceExecution` and `DurationReport` actions: ResourceExecution uses its
exact fresh completion/progress/score verifier; DurationReport keeps its own
one-shot preservation verification and enters HumanRequired after an
ambiguous write without being mistaken for the resource goal. Submission
remains a separate single-action Draft/receipt/verification flow.

## One-hundred-and-seventy-sixth Phase 0 slice

Capture is now an executable first-batch authentication boundary rather than a
metadata-only version marker. An Authentication implementation returns a
validated, versioned recipe containing its exact HTTPS start URL and origin
allowlist, polling bound, method/session kind, credential purposes and ordered
request-header, browser-storage, Cookie or atomic JSON sources. Registry rejects
version-only claims, undeclared methods, invalid origins, duplicate purposes,
unbounded recipes and recipes with no required output.

Auth Bootstrap creation freezes the implementation-backed recipe version and a
successful claim returns that exact recipe. Credential submission is checked on
both sides: the helper refuses a method, session kind or field set that differs
from its claimed recipe, while Engine re-resolves the Provider implementation
and rejects recipe drift before Provider validation or any SecretStore write.
OpenAPI exposes the complete typed recipe union and all Bootstrap success/event
responses; Capture no longer has an untyped deferred-operation exception.

`asterism-capture automatic` launches one visible Chromium/Edge instance with a
unique temporary profile and random loopback DevTools port. Its bounded
WebSocket client attaches only to an allowlisted page, accepts only declared
browser facts, binds request headers to origin and top-level document loader,
handles CDP header-event reordering, confirms the document did not change during
one snapshot, zeroizes transport/JSON buffers and submits only when every
required output resolves atomically. Startup failures, cancellation, normal
completion and CDP close all enter one process-tree/profile reclamation path.
Unit/contract suites and an ignored real-browser smoke test cover both successful
allowlisted attachment and zero leftover helper processes/profiles; Provider
account validation against real platform sessions remains the later live gate.

## One-hundred-and-seventy-seventh Phase 0 slice

Multi-capability Task execution is now an explicit Provider-planned durable
workflow. `TaskExecutionCapability::execution_plan` rejects combinations by
default; a Provider must return the exact phase order for an evidenced action
set. Core validates that order as a permutation of the caller's canonical
selection and persists every phase in the same transaction as the Execution
and scheduler Job.

The worker marks one phase `issued` with the current Attempt binding before its
remote call, runs only that singleton Capability, applies its own verification
contract, and marks it `succeeded` before advancing. A crash or network error
after issue enters verification-only recovery rather than replaying the
possibly accepted mutation. Recovery verifies the exact issued phase; after a
confirmed phase it schedules a new Attempt for the next still-pending phase,
while phases without a readback path become HumanRequired instead of being
guessed or repeated. This closes the WELearn donor order of DurationReport
before completion/progress/score ResourceExecution without collapsing their
different success meanings.

Provider-specific remote-state exceptions are also explicit rather than
global. A TaskExecution implementation may opt an exact action set into an
audited state such as WELearn `NotOpen`; the default grants nothing and Core
still refuses `Expired` and `Removed`. Shared Provider/account concurrency
schema bounds now reach the donor-evidenced finite maximum of 100 while the
worker continues to enforce global, Provider and account admission leases.
Unit/all-course batch planning and Cidaren's question-internal continuation
steps remain separate shared orchestration contracts; neither is hidden inside
Provider-private scheduling or misrepresented as a TaskCapability phase.

## One-hundred-and-seventy-eighth Phase 0 slice

Capture navigation authority and credential-read authority are now distinct.
An exact third-party OAuth origin may be listed for top-level navigation
without granting access to its request headers, storage or Cookies; every
declared Secret source must belong to the narrower `read_origins` set. Recipe
versions changed with this wire contract, so an older helper cannot silently
interpret the widened navigation set as read authority.

Credential completeness alone is no longer the only readiness contract.
Recipes may require an exact request, or an exact 2xx response media type, on
the current top-level document loader. The Chromium helper correlates bounded
request IDs to method, canonical origin/path, loader, status and MIME type and
accepts credentials only when both the frozen gate and all required outputs
hold in one stable snapshot. WELearn uses the authenticated Course-list route
with `application/json`; an anonymous session's pre-login Cookies and 200 login
HTML therefore cannot close the browser or be submitted as a false session.

Provider execution requests also carry the immutable Core `ExecutionId`.
Providers can derive bounded random-duration or score choices per Execution
while keeping retries and verification stable; unlike Task-only hashing, a
future authorized Execution of the same Task receives a distinct identity.

## One-hundred-and-seventy-ninth Phase 0 slice

BrowserBridge is now reachable through the first shared Core/API boundary
instead of existing only as a registry slot. `BrowserSessionSpec` has an exact
Provider wire revision and validates a bounded credential-free isolation key,
unique canonical HTTPS navigation origins and a finite serialized shape.
Navigation authority does not imply credential injection authority.

The owner-scoped Task service requires the persisted Task to advertise
BrowserBridge, rebinds the same authenticated Provider account, passes only
opaque credential references, invokes the Provider's fresh detail-backed
session policy and rejects unsafe output before returning it. The corresponding
`GET /api/v1/tasks/{task_id}/browser-session-spec` route and generated OpenAPI
schema expose this policy without launching a browser, injecting a Secret,
performing DOM actions or claiming Task completion. Durable helper sessions and
the internal typed Provider command/result ledger are now shared Core
boundaries; actual helper dispatch, result transport, credential commit and
verification-only recovery remain separate execution slices rather than being
falsely implied by this read boundary.

## One-hundred-and-eightieth Phase 0 slice

External browser OAuth now has a shared one-shot Core boundary. A Provider
returns a bounded authorization URL plus redacted state/context digests; Core
persists only those digests in an owner/account/AuthSession-bound short-lived
pending record. Callback submission atomically claims that record before the
Provider exchange, passes the callback URL only as an in-memory Secret, then
uses the existing Provider validation and atomic credential commit boundary.
The callback itself, authorization code, state and runtime crypto context are
never persisted or returned by the status API.

Pending callback state is distinct from `AuthSession` state and from the
credential receipt. `Pending -> Completing` is single-use; Provider/network
ambiguity consumes the callback and cannot trigger an automatic replay.
Owner-scoped status reads run a transactional fail-closed reconciliation: an
authenticated session completes a stranded pending record as `Succeeded`,
while a stale in-flight exchange becomes `Ambiguous` and
`ProviderUnavailable` without invoking the Provider again. The OpenAPI surface
therefore exposes only sanitized state, expiry, consumption and revision, and
marks the submitted callback URL as write-only.

## One-hundred-and-eighty-first Phase 0 slice

Capture acquisition is no longer restricted to one fixed recipe per
Provider. `AuthenticationCapability` may return a bounded ordered set of
complete alternative recipes; registry validation requires unique positive
versions, validates every recipe independently and keeps the metadata version
as the default first choice. Alternatives describe separate atomic credential
bundles and may differ in authentication method, session kind, readiness and
output shape. A helper may not merge fields or browser snapshots across them.

Authenticated clients can discover the ordered alternatives through
`GET /api/v1/providers/{provider_id}/capture-recipes` and may select a specific
`recipe_version` when creating a bootstrap session. Core freezes that exact
version into the durable session, returns only the matching recipe at claim,
and validates the submitted credential bundle against the same version before
any Provider validation or Secret write. Omitting the selection preserves the
Provider's audited default recipe. This permits Cidaren's token-only and
Composite Capture routes to coexist without weakening recipe/session binding.

## One-hundred-and-eighty-second Phase 0 slice

`BrowserBridge` now has a durable helper-session and pairing boundary instead
of ending at the credential-free Provider policy read. Session creation first
re-runs the owner-scoped Task/Provider lookup, then atomically freezes the
owner, Provider account, Task, Provider implementation version and validated
policy digest. The bounded policy is stored beside that digest; any corrupted
or drifted record fails closed on decode.

Core returns the random pairing token exactly once and persists only its
SHA-256 digest. A helper claim consumes that digest through one compare-and-set
transition, rotates it to an independent access-token digest and cannot be
replayed. Access is path-bound to the same claimed session. Expiry or owner
cancellation invalidates both token families, while every transition preserves
revision and sanitized Audit attribution. Public helper responses omit owner
and Provider-account identifiers and expose neither credential references nor
browser storage, DOM or response payloads.

The protected API can create a helper session from an executable Task, read an
owner-scoped snapshot and cancel it. The helper API can claim once and poll the
exact frozen policy under the `BrowserBridge` authorization scheme. OpenAPI
describes all four operations and one-time sensitive response fields. This
slice establishes identity, policy and token transport; typed Provider
commands/results now have an internal bounded type/digest ledger. Public helper
command delivery, credential injection, durable Artifact/Attempt binding and
verification-only recovery remain explicit following slices rather than an
arbitrary-script surface.

## One-hundred-and-eighty-third Phase 0 slice

BrowserBridge exchanges now have a shared durable command/result boundary.
Core accepts only a positive contiguous sequence, a bounded lowercase message
type and a non-zero SHA-256 digest for each Provider-validated typed payload.
SQLite binds each Core-issued command to a live claimed BrowserBridge session,
then accepts results only through that session's helper access token. It permits
an identical retry without replay, rejects sequence or immutable command/result
conflicts, and stores only sanitized audit metadata. No public endpoint is
advertised until Core can deliver the actual typed payload rather than accept a
helper-supplied digest as command authority. Provider-specific Cidaren Capture
and UAI residence envelopes remain in their Provider crates; helper dispatch,
result transport, credential commit, DOM execution,
durable Attempt/QuestionSession binding and fresh verification continue as
separate Core work.

## One-hundred-and-eighty-fourth Phase 0 slice

Chaoxing Exam Question acquisition now uses the shared durable pre-Question
attempt contract instead of hiding `phone/start` inside a read call. The
Provider first performs Course/Exam/cover reads and encodes an encrypted,
Task-bound command. Immediately before execution it repeats those read-only
lookups and requires the full continuation digest to remain identical. Core
persists the exact `chaoxing.exam-start.v2` request digest before the Native
transport sends the one-shot request, so a network ambiguity cannot trigger an
automatic replay.

A validated start redirect yields the current attempt identity, dynamic `enc`
and timing fields. The Provider fetches the exact attempt-bound mobile page,
normalizes every Question, fingerprints the complete ordered set and returns a minimal
encrypted artifact. SQLite atomically materializes that artifact with the
immutable `QuestionSnapshot`, `QuestionSession` and accepted operation record;
HTML and answer content never enter the artifact. Exam-code, face and captcha
branches remain typed `HumanRequired` for the pending BrowserBridge/Capture
execution path, and ambiguous start recovery remains locked until fresh
readback behavior is proven rather than guessed.

## One-hundred-and-eighty-fifth Phase 0 slice

Submission scheduling now atomically claims an optional `QuestionSession` from
the exact immutable Draft snapshot. The scheduling transaction first inserts
the Execution, then validates the session's owner/account/Task/Snapshot,
Provider/version, initial encrypted continuation digest and expiry before
binding both session and continuation to that Execution. The Task transition,
credit reservation, Execution, scheduler Job and claim therefore commit or
roll back together.

Submissions whose Question snapshots carry no Provider artifact retain the
existing path. When an artifact-bearing session exists, stale, foreign,
expired, already claimed or digest-drifted state rejects scheduling and leaves
the Task ready with no Execution or Job. This closes the persistence gap before
the following worker slice resolves the claimed continuation and records each
Provider mutation command before dispatch.

## One-hundred-and-eighty-sixth Phase 0 slice

Artifact-bearing submissions now run through a shared durable operation loop.
The Provider receives the exact encrypted continuation selected by the
immutable Draft and freezes one bounded operation type, request digest and
optional delay at a time. Core persists that operation as `issued` before any
remote dispatch. A definite response atomically marks the operation accepted
and rotates the encrypted continuation; a final step separately yields a
Receipt and still requires fresh Submission verification.

An `issued` operation found after interruption is first made `ambiguous` and
can never be sent again. Recovery may rotate it only when the Provider's fresh
readback returns a definite continuation bound to the same operation digest.
No readback result keeps the operation ambiguous and defers or requires human
review instead of falling through to whole-submission verification. The
original QuestionSession deadline remains authoritative and cannot be extended
by a Provider continuation.

After fresh verification confirms the exact Draft as remotely completed,
SQLite consumes the claimed QuestionSession in the same transaction that
finishes the Execution. That terminal transition requires every registered
operation to be accepted and the current continuation revision to be the
rotation produced by the latest operation. Legacy submissions without a
QuestionSession retain their existing one-shot Submit/verify path.

## One-hundred-and-eighty-seventh Phase 0 slice

The ordinary read-only Question path can now retain Provider runtime material
without pretending that parsing was a pre-Question mutation. `QuestionParse`
returns one complete validated set and an optional Provider-scoped encrypted
continuation. Artifact-free Providers inherit the existing per-reference
parser behavior unchanged.

When an artifact exists, SQLite atomically persists the immutable
`QuestionSnapshot`, an unclaimed `QuestionSession`, the encrypted artifact and
its digest/type/phase metadata. A partial snapshot or plaintext route cannot
escape if any binding or Secret write fails. The session remains bound to the
owner, Provider account, Task, Provider version and exact snapshot, and the
original short expiry is authoritative.

Provider-native `AnswerResolve` may ask Core for the active artifact belonging
to that exact owner and snapshot. Core decrypts it only through the
Provider-scoped repository and passes a borrowed redacted continuation to the
Provider method; Domain Questions, candidates, Drafts and API responses still
contain no runtime URL, token or crypto material. Snapshots without an artifact
continue through the legacy resolver method.

## One-hundred-and-eighty-eighth Phase 0 slice

Post-Question execution now distinguishes three real Provider outcomes instead
of forcing every accepted response into continuation rotation. `Continue`
rotates the encrypted state for another operation on the same immutable Draft;
`NextQuestion` carries a complete newly observed Question set and its bounded
Provider artifact; `Submitted` is a definite terminal acknowledgement and does
not invent a replacement continuation. `NormalizedAnswer::Skip` separately
expresses an explicit audited skip command, so a Provider can never interpret
`Unknown` or a missing selection as mutation authority.

For `NextQuestion`, SQLite accepts the issued operation, persists the next
immutable `QuestionSnapshot`, creates its active unclaimed `QuestionSession`
and encrypted continuation, consumes the old claimed session, and records the
old-to-new transition in one immediate transaction. Recovery resolves that
transition before any Provider call, so a crash after the remote response can
finish locally without replaying the prior non-idempotent request. The current
Execution succeeds, while its Task atomically returns to `ready`; the new
Question must pass answer resolution and immutable Draft construction again.

A terminal `Submitted` operation may be accepted without rotating Provider
state, but its Receipt still is not success. Fresh `SubmissionVerify` must
confirm the exact old Draft before the claimed session and Task become
terminal. Migration 041 makes next-session transitions durable and binds each
one to the exact prior session, operation sequence, Execution, next session,
snapshot and accepted timestamp.

## One-hundred-and-eighty-ninth Phase 0 slice

Chaoxing's durable Exam session now continues past start and Question
materialization into the audited mobile save/final protocol. The one-shot start
reads the full attempt preview and treats its rotated `enc`, remaining time and
last-update time as authoritative. The encrypted v2 artifact binds that state,
the complete ordered Question fingerprint and an exact next-answer cursor to
the immutable Draft.

For each Question, the Provider freezes CxKitty-compatible signature fields,
query and form body into `chaoxing.exam-answer-save.v1`; Core persists its
digest before one POST. An accepted response must rotate bounded monotonic
attempt state and exactly one cursor before the continuation can advance.
After the final cursor, a separate `chaoxing.exam-final-submit.v1` produces only
a Receipt. Temporary-save ambiguity remains non-replayable. Final-submit
ambiguity can be recovered only when a fresh exact Exam inventory row is
already Completed.

Session-aware verification decodes and rebinds the last continuation, requires
all answer saves to have completed, repeats the value-free Draft preview, and
then reads fresh Exam inventory. Only Completed confirms the submission; every
Question remains Unverified without independent result-page answer evidence.
Synthetic save/final fixtures and an end-to-end Provider operation-loop test
cover the two continuation rotations, terminal receipt and fresh verification.

## One-hundred-and-ninetieth Phase 0 slice

BrowserBridge command authority now survives daemon restart without trusting a
helper to echo Provider-private JSON. Core accepts the exact bounded command as
a zeroizing `SecretValue`, verifies that its SHA-256 digest is the immutable
exchange identity, and stores the exchange plus an XChaCha20-Poly1305 encrypted
command artifact in one immediate SQLite transaction before dispatch.

The encrypted repository is permanently scoped to one Provider at composition.
Resolution repeats the persisted session's owner, Provider account, Task and
Provider binding, authenticates the Core/Provider-runtime secret access, loads
the exact sequence and verifies the decrypted bytes against the exchange
digest. Missing artifacts from the earlier metadata-only schema, cross-Task
lookups, ciphertext corruption and key mismatch all fail closed. Identical
issuance is idempotent only when the encrypted artifact association already
exists; a digest-only legacy row cannot be upgraded by an ambiguous retry.

Migration 042 adds the restricted one-to-one Secret reference while preserving
old rows for deterministic recovery decisions. Engine command issuance no
longer exposes the metadata-only storage entry point. Provider-specific Cidaren
Capture and UAI residence artifacts can now enter this shared boundary; helper
dispatch, typed result transport and the atomic terminal-result/credential
replacement transaction remain the next shared BrowserBridge slices.

## One-hundred-and-ninety-first Phase 0 slice

A credential-producing `BrowserBridge` result now has one terminal transaction
instead of separate exchange, Secret and session writes. Storage reauthenticates
the exact helper-token digest against the live claimed session, repeats the
owner/account/Task/Provider binding, requires the issued sequence to reference
an encrypted command artifact, and compares the immutable command metadata
before accepting the Provider-validated completed exchange.

Within the same immediate SQLite transaction, Core records the terminal result,
replaces the complete encrypted Provider credential set, marks the bound account
authenticated, advances the helper session to Completed and clears both token
families. Any command mismatch, absent artifact, stale session update, account
binding conflict or Secret failure rolls the entire operation back. A lost
response cannot replay credential replacement through the now-invalid helper
token; recovery reads the already terminal ledger instead.

Engine owns helper-token hashing and exposes only the validated-bundle commit
request. Provider code still cannot access Storage, choose another account or
declare a helper echo as command authority. This closes Cidaren's shared atomic
Capture commit gap; generic typed helper dispatch and result transport remain a
separate bounded slice.

## One-hundred-and-ninety-second Phase 0 slice

Raw helper results now enter an encrypted Provider-scoped inbox before parsing.
Migration 043 binds one bounded result type, SHA-256 digest, encrypted Secret
blob and first-received timestamp to the exact BrowserBridge session/sequence.
The helper token must still authenticate the live claimed session, the issued
command must already have its encrypted artifact, and conflicting retries cannot
replace the first result. Identical retries return the original durable metadata.

Core can resolve every retained result by owner, Provider account, Task,
Provider and sequence after restart. Decryption authenticates the Secret AAD and
recomputes the exact result digest before Provider code sees any raw JSON,
credential or opaque browser handle. Ciphertext corruption and foreign bindings
fail closed. A terminal exchange, including the credential-producing Capture
transaction, is accepted only when its result type and digest match this inbox.

This deliberately keeps receipt separate from Provider acceptance. A Provider
may recover the prior command plus raw result, validate it, and then derive the
next self-contained encrypted command through the existing issue-before-dispatch
boundary. UAI may retain accumulated menu/Tab/Task and budget cursor state inside
its opaque command artifact or reference earlier result sequences; Core does not
interpret Provider DOM state and does not need an oversized complete-and-next
transaction.

## One-hundred-and-ninety-third Phase 0 slice

The generic `BrowserBridge` helper transport now preserves the non-replayable
command boundary across HTTP. Migration 044 records the first dispatch of one
session/sequence under a unique primary key. Storage authenticates the live
claimed helper token, Provider scope, encrypted command artifact and command
digest in the same immediate transaction that inserts this dispatch record.
Only that first transaction returns the opaque command bytes; a repeated GET
returns a conflict and cannot replay a browser action after an ambiguous lost
response.

The helper API exposes exact command bytes as bounded
`application/octet-stream` with type and digest headers. Result upload is a
separate bounded binary route with an explicit result type. A raw result is
accepted only after the matching dispatch exists, then enters the existing
encrypted first-writer-wins inbox. Identical uploads return the original
receipt, while different bytes or types for the same sequence fail closed.
Command and result bodies remain zeroizing owners at the Core transport
boundary and are never represented as ordinary loggable JSON DTOs.

Dispatch, receipt and Provider acceptance remain distinct facts. The API does
not mark an exchange complete, interpret Provider DOM state, derive credentials
or issue a successor command. Provider runtime must resolve the encrypted
command/result pair, validate its own protocol and fresh bindings, and only then
use the existing terminal exchange or atomic credential-commit boundary.

## One-hundred-and-ninety-fourth Phase 0 slice

A `BrowserBridge` session now freezes the exact credential-free HTTPS route the
isolated helper opens first. The Provider must derive this route from the same
fresh Task detail used to authorize the capability; Core bounds and parses it,
rejects non-HTTPS or userinfo-bearing routes, and requires its exact origin to
belong to the session's immutable navigation allowlist. The route participates
in the existing specification digest, durable JSON snapshot and helper claim
response, so a restart or later Provider change cannot redirect an already
created session.

This is navigation authority, not credential injection and not command
authority. The helper still receives no Provider credential through the
session specification, and no browser action can run until Core has durably
issued and one-shot dispatched its exact encrypted command. Provider policy
versions advance when adopting the new route binding so old and new session
shapes cannot be mistaken for one another.

## One-hundred-and-ninety-fifth Phase 0 slice

The helper's actual browser document identity is now a separate immutable fact.
After consuming the pairing token and opening the frozen start route, the helper
may bind one exact HTTPS origin and one bounded opaque frame ID through its
session-scoped access token. Migration 045 stores the first origin/frame pair
with its observation time under the session primary key. Storage authenticates
the still-live claimed session, repeats the owner/account/Task/Provider binding,
requires the origin to belong to the frozen policy and writes an Audit record in
the same immediate transaction.

An identical retry returns the original observation time. A foreign token,
changed frame, changed origin, pre-claim timestamp or navigation outside the
allowlist cannot replace the first writer. Owner-scoped recovery can read the
binding after restart, but the helper cannot choose another session or mutate
the Provider policy. Exact command issuance now requires this durable runtime
binding to exist, ensuring Provider command construction never relies on a
caller-only frame or origin value.

The runtime binding still grants no remote mutation by itself. It carries no
credential, command bytes or completion state; every browser action remains an
independent encrypted exchange that Core persists before its one-shot dispatch.

## One-hundred-and-ninety-sixth Phase 0 slice

BrowserBridge command issuance can now atomically retain one optional
Provider-private runtime-state sidecar. Migration 046 binds a bounded state
type, SHA-256 digest, encrypted Secret and issuance timestamp to the exact
session/sequence exchange. Storage validates the command and state bytes
independently, requires both identities to match the same issued exchange, and
writes both encrypted artifacts in the same immediate transaction. A duplicate
issuance succeeds only when the command and optional-state shapes are identical;
adding, removing or changing a sidecar after the fact is a sequence conflict.

Owner/account/Task/Provider-scoped recovery decrypts both artifacts and verifies
their independent digests before Provider runtime receives them. Ciphertext
corruption or metadata drift fails closed. The dispatch path deliberately uses
a command-only result type and never queries or decrypts runtime state, so a UAI
cursor, accumulated batch identity or future Provider recovery artifact cannot
cross the helper transport boundary.

The sidecar is not a scheduler or authority by itself. UAI still requires Core
to freeze the Course batch owner and start ordinal, then rebuild fresh ordered
inventory before its Provider can validate the cursor and issue sequence `n+1`.
Cidaren commands need no sidecar and retain the existing command-only shape.

## One-hundred-and-ninety-seventh Phase 0 slice

Core can now reconstruct the latest BrowserBridge runtime as one owner-scoped
snapshot. Storage resolves the latest exchange sequence only after rebinding the
session owner, Provider account, Task and Provider. Engine combines that record
with the frozen session policy, immutable origin/frame binding, exact encrypted
command, optional Provider-private state and optional encrypted raw result.

Recovery fails closed when a runtime binding, command artifact or terminal raw
result is missing, or when any resolved artifact names a different exchange.
An issued exchange may legitimately have no result while the helper is still
running; it may also have a durably received result awaiting Provider parsing.
A terminal exchange must retain the raw result that justified Provider
acceptance. No access token, command bytes or cursor is reconstructed from
helper input.

This snapshot supplies deterministic process-restart ownership but does not
advance a Provider state machine. A Provider adapter must still freshly rebuild
and validate its protocol authority, then either accept the prior result and
issue the exact next sequence or stop for typed recovery/human handling.

## One-hundred-and-ninety-eighth Phase 0 slice

Core now retains an ordered issue/receipt ledger for each remote mutation inside
one Provider-owned atomic operation. Migration 047 binds every ordinal to the
exact Execution attempt, claimed scheduler job and worker, plus a bounded
operation type and request digest. The first ordinal must be one, gaps are
rejected, and Core will not issue ordinal `n + 1` until ordinal `n` has a
durable, explicit boolean receipt and response digest.

Issuance and receipt both revalidate the live scheduler claim, Execution lease
and active attempt in an immediate transaction. Exact retries return the
original record, while changed request or response identities fail closed.
Request and response material never enters the ledger or Audit metadata; only
their digests are retained and Audit represents them as hashed values.

An issued row without a receipt is deliberately ambiguous. Recovery may inspect
the attempt ledger, but neither Core nor a Provider may infer rejection, skip
the missing response, or replay that ordinal. This is the persistence boundary
needed by WELearn's multi-request duration completion transport; injecting it
through the Provider execution API remains a separate contract slice.

## One-hundred-and-ninety-ninth Phase 0 slice

The execution runtime now injects the atomic mutation ledger through a narrow
Provider API sink. Provider code supplies only an ordinal, a namespaced bounded
operation type and request digest before a mutation, followed by a response
digest and explicit acceptance bit after parsing a definite response. Engine
retains the Execution, attempt, scheduler job, worker and correlation binding;
those ownership identifiers never enter Provider code.

The sink is available only during Core's persisted `TaskExecutionCapability`
call, so unit fixtures, read-only callers and the independently durable
Submission path do not accidentally claim this authority. Engine rejects
operation types outside the current Provider namespace. Only a newly issued row
or newly recorded receipt succeeds: encountering an existing issue or receipt
fails closed into read-only/human recovery rather than granting a new native
session permission to replay or skip a request.

Claim loss is still surfaced to the runner even when Provider code observes a
sanitized sink error. A receipt persistence failure is treated as potentially
post-mutation and therefore ambiguous. WELearn can now adapt its Provider-owned
atomic duration sink to this generic boundary; registration remains blocked
until every start/keep/set/save transition actually uses it.

## Two-hundredth Phase 0 slice

Composite execution now freezes Provider call boundaries independently from
capability order. `TaskExecutionCapability` resolves groups from the selected
capabilities and already frozen runtime settings, and may group adjacent
capabilities only when those scheduling-time inputs uniquely select one
evidenced remote transaction implementing the whole group. It may not guess
from capabilities when a fresh Provider detail or batch profile would change
the flow. Core validates that the flattened groups are an exact permutation of
the authorized selection, then persists each step's call and member positions
atomically with the Execution. Migration 048 backfills every older step as its
own call.

Storage issues or succeeds every member of a grouped call in one immediate
transaction under the same active attempt, scheduler claim and worker lease.
No member may become issued alone, a later call remains blocked until every
earlier call succeeded, and a changed group shape fails closed. Engine derives
the exact contiguous capability slice from those durable rows, invokes the
Provider once, requires the Provider's explicit whole-group verification
contract when advertised, and marks all members successful together.

Recovery never reconstructs group authority from current Provider defaults.
An issued grouped call is reloaded from the frozen rows and can only take the
read-only whole-group verification path; it is not automatically invoked again.
WELearn may now declare `DurationReport + ResourceExecution` as one call, but
its profile/target and current-donor preserved-time goal still need their own
durable Provider-private execution evidence before post-crash verification can
prove the exact goal.

## Two-hundred-and-first Phase 0 slice

Provider execution planning can now return one optional credential-free private
artifact together with its call groups. Core bounds the artifact type to the
current Provider namespace, accepts only a sanitized JSON object of at most 64
KiB, rejects credential-shaped keys, computes a domain-separated digest and
redacts both payload and digest from `Debug` output.

Migration 049 stores that exact Provider, type, digest, payload and capture time
under the Execution primary key in the same scheduling transaction as the
immutable capability calls and resolved runtime settings. Recovery reads repeat
the Task-to-account Provider binding, require the capture time to equal the
Execution creation time, reconstruct the bounded artifact and recompute its
digest. Changed payload, type, Provider or digest fails closed.

This artifact contains planning evidence, not credentials and not permission to
mutate. It is not yet injected into `ExecutionRequest`; that remains a separate
contract change so every Provider adapter must explicitly accept and validate
the new frozen input. WELearn's atomic child plan and UAI's Course batch/start
plan can use this boundary once their adapters implement that contract.

## Two-hundred-and-second Phase 0 slice

Every Provider execution and verify-only recovery request now carries the
optional scheduling-time plan artifact. Engine reloads it through Storage's
Provider, capture-time and digest checks while preparing the call, repeats the
runtime account binding, and supplies the exact typed value without widening
the active capability slice. Recovery therefore cannot silently reconstruct a
different private goal from current defaults, and a corrupt artifact fails
before Provider code runs.

The private artifact is deliberately skipped by generic `ExecutionRequest`
serialization and its existing `Debug` representation remains redacted. It is
read-only evidence, not credential material or mutation authority. A Provider
that owns an artifact must validate its namespaced type and decode/rebind the
payload against fresh remote facts before use; Providers without one receive
an explicit `None`.

This does not authorize Core to synthesize Course batch evidence from one Task.
WELearn and UAI child plans require complete, freshly validated Course/Task
membership plus an exact start/target decision. Their future batch scheduling
entry must freeze those inputs first and then pass the resulting artifact into
this already durable execution boundary.

## Two-hundred-and-third Phase 0 slice

Execution planning now has an asynchronous read-only Provider hook before Core
freezes the Execution. Core supplies a preallocated stable Execution identity,
the exact local and remote Task/Course binding, canonical requested capability
set, immutable resolved settings and a Provider/account context containing only
opaque credential references. The default hook preserves the prior synchronous
single-Task behavior, so Providers opt into fresh discovery explicitly.

The hook grants no mutation authority. A Provider may use it only to rediscover
the complete remote facts needed to select call groups and produce one bounded,
credential-free plan artifact. Core repeats Provider/account binding before the
call, validates the returned call permutation and artifact namespace, then
persists the result in the same scheduling transaction as capability steps and
runtime settings. An idempotent replay returns the existing Execution before
calling the Provider planner again.

The API integration fixture exercises the complete boundary rather than only a
trait default: planning receives the stable Execution/Task identities, returns
a private artifact, and scheduling stores its exact type and payload under that
Execution. This hook makes fresh Provider-owned batch preparation possible, but
the public Course/Unit selection and parent/child Execution contract remains a
separate orchestration slice.

## Two-hundred-and-fourth Phase 0 slice

The local Capture library now implements the complete outbound BrowserBridge
helper transport already exposed by Core. It consumes a pairing token once,
keeps the rotated access token only in zeroizing memory, validates the frozen
session/spec binding and permits only an exact allowlisted origin plus bounded
frame identity before command retrieval.

Command dispatch is sequence-bound and deliberately non-replayable. The client
accepts only a Provider-namespaced type, canonical SHA-256 header and non-empty
binary body up to 256 KiB, recomputes the digest before exposing redacted
`SecretValue` bytes, and refuses another command while one result is active. A
failed command GET is never automatically retried because Core may already have
marked the command dispatched.

Result submission retains the active sequence, Provider namespace and bounded
zeroizing body. An ambiguous POST may repeat the exact type and bytes because
Core's existing first-writer digest comparison returns a duplicate receipt;
changed evidence conflicts. Unauthorized responses clear the in-memory access
token, pairing and access token shapes cannot substitute for each other, and
command/result digests plus artifacts remain redacted from `Debug` output.

This closes the reusable helper-side HTTP transport, not Provider action
execution. A subsequent slice must connect typed Provider command handling and
actual DOM/browser actions to this stateful client; no arbitrary JSON or
selector interpreter is introduced by the transport layer.

## Two-hundred-and-fifth Phase 0 slice

The Capture runtime can now launch a BrowserBridge session from the exact
frozen `BrowserSessionSpec`. It validates the policy, creates a unique temporary
profile, applies the Provider's visible/headless choice, opens only the exact
credential-free start route and attaches DevTools solely to a page whose HTTPS
origin is in the frozen allowlist.

Capture and BrowserBridge now share one bounded CDP initialization and stable
top-level document parser without sharing credential-read authority. The
BrowserBridge path configures no declared header/storage/Cookie sources and
returns only the current allowlisted origin plus bounded frame ID needed by the
durable runtime-binding API. Every binding read repeats process liveness and
navigation policy; an origin escape fails before command dispatch.

Normal shutdown uses the existing bounded DevTools close and process-tree plus
temporary-profile reclamation path. An ignored real-Chromium smoke test covers
headless launch, exact target selection and origin/frame binding when a local
browser and network are available. Typed Provider DOM action handlers remain
the next layer; this slice does not execute opaque command JSON.

## Two-hundred-and-sixth Phase 0 slice

`BrowserSessionSpec` now freezes a separate credential-free browser read
policy before Chromium starts. Navigation authority still grants no access to
browser state: every permitted request header, local-storage key or
session-storage key names one exact allowlisted HTTPS origin. The bounded list
is duplicate-free, request-header names are canonical lower case, and the
policy participates in the durable spec digest. Empty policies retain the old
serialized shape and digest for already frozen sessions.

The Capture BrowserBridge registers only predeclared request-header pairs with
CDP, so unrelated headers are discarded as events arrive rather than cached
for later interpretation; an early extra-header event without a known
request-origin binding is discarded as well. Each typed dispatch selects a
duplicate-free subset of the frozen policy. A read snapshot copies only those
selected observations from the current top-level loader and reads only the
selected storage keys for the current origin through fixed literal
expressions. The document binding is checked again after acquisition;
navigation during the read invalidates the entire snapshot.

Snapshot values remain secret, consuming and debug-redacted. This boundary
does not add Cookie access, arbitrary JavaScript, Provider-supplied selectors
or late permission expansion. Typed Provider handlers must still validate the
opaque command envelope and select from their compiled action/source profiles
before any browser operation or result construction.

## Two-hundred-and-seventh Phase 0 slice

The Capture transport can now poll Core's authenticated BrowserBridge snapshot
and requires the session plus frozen specification to remain byte-for-byte
consistent with the original claim. A changed Provider, revision, lifetime or
policy fails before another command is requested.

Command retrieval now distinguishes an explicit server-side `404` from a
dispatch. That status means the exact next sequence has not been issued and is
safe for a bounded runner to poll again. A successful response still creates
one active command, while `401`, `409`, malformed evidence and every transport
failure remain hard errors. In particular, the helper never retries after an
ambiguous GET because Core may already have durably marked the command as
dispatched.

This supplies the waiting semantics for a long-lived local runner without
inventing command replay. Provider result projection, DOM execution and Core's
post-result orchestration remain separate typed boundaries.

## Two-hundred-and-eighth Phase 0 slice

The Capture library now links the Cidaren Provider's compiled helper contract
instead of interpreting its command JSON. It transfers ownership of the Core
command artifact into Cidaren's digest/session/origin/frame/sequence projector,
then derives the requested browser read subset only from the resulting closed
TokenOnly or Composite source enums.

Each poll acquires a new stable-document read snapshot. The handler selects the
first available UserToken alternative and, for Composite mode, the first
login-info alternative from that same snapshot. It never combines values from
different polls, rejects an origin/frame change after command projection and
stops at the frozen session expiry. Missing facts cause a bounded local wait;
unsafe or malformed facts fail through the Provider result validator.

The completed local observation is encoded by Cidaren into its exact result
type and zeroizing artifact before it returns to the shared inbox transport.
TokenOnly cannot include storage context, Composite cannot omit login-info,
and no Cookie, user-session, selector, script or arbitrary field is accepted.
Core-side recovery, fresh Provider validation and the atomic result/credential
commit remain the next orchestration boundary; this helper checkpoint does not
claim server-side acceptance.

## Two-hundred-and-ninth Phase 0 slice

Core now owns the durable server-side path from a raw BrowserBridge credential
result to an authenticated account replacement. Providers expose a closed set
of exact credential-terminal result types in addition to classifying each
type. Storage uses the Provider ID and that set in the bounded inbox query, so
unknown, intermediate and execution-terminal results cannot be parsed as
credentials or starve the credential processor.

For every selected inbox entry, Core recovers the frozen session policy,
runtime binding, exact encrypted command and exact encrypted result under a
Core service identity. It freshly rebinds owner, account, Task, Provider and
implementation version, rejects runtime sidecars on credential-terminal
commands, asks the Provider to validate the recovered artifacts, and runs the
replacement bundle through the existing authentication validator. Only the
Provider-completed exchange whose type, digests, sequence and timestamps match
the durable result can cross the commit boundary.

The final Storage transaction no longer needs the helper's plaintext access
token after restart. It rebinds the claimed session and account itself, writes
the encrypted credential replacement, completes the exact exchange and
session, clears helper access, and appends audit evidence atomically. The HTTP
result receipt remains a separate idempotent inbox operation, so an ambiguous
response never causes credential validation or terminal mutation replay. The
daemon runs this processor only when a keyring and an explicitly declaring
Provider are configured; individual failures stay durable for a later tick.

## Two-hundred-and-tenth Phase 0 slice

BrowserBridge credential-result processing now has its own durable attempt
ledger instead of relying on repeated inbox reads. A worker claims one
Provider/result-type item per Provider tick under `BEGIN IMMEDIATE`, increments
its attempt once and stores its identity plus an expiring lease. Another daemon
cannot process the same result while that lease is live; after a crash, the expired
claim becomes eligible for a fresh numbered attempt.

Failed recovery, Provider validation and credential persistence use the shared
bounded retry policy and persist the next-attempt time plus a non-secret error
class. Exhausted attempts and binding/sequence conflicts become dead letters,
which no longer occupy the pending query. Claim limits, lease duration,
attempt budget, worker labels and error labels are all bounded and validated;
the migration also caps persisted attempts independently of daemon config.

The processing lease is deliberately separate from the helper access token
and from the exchange itself. It grants no browser or secret authority, and a
successful atomic credential/session commit remains the only path that marks
the exchange completed. This preserves exact result receipt and restart
recovery while preventing malformed terminal artifacts from creating a hot
retry loop.

## Two-hundred-and-eleventh Phase 0 slice

A credential-result dead letter now terminates its helper authority in the
same immediate transaction. Storage first proves that the result is still
owned by the expected processing worker, then transitions the bound claimed
session through the Domain `Failed` state, clears both pairing and access-token
hashes, and writes a sanitized worker audit record. A concurrent completion or
session transition rolls back the dead letter as well, so result and session
terminal states cannot disagree.

The same transaction helper handles an expired lease at the database's final
attempt cap. Crash recovery therefore cannot leave an exhausted result hidden
from future claims while its helper token remains usable. Retryable failures
continue to release only the processing lease and persist `next_attempt_at`;
they deliberately keep the short-lived claimed session available for the next
bounded attempt.

## Two-hundred-and-twelfth Phase 0 slice

The BrowserBridge Provider contract now declares three separate closed result
type sets: credential terminal, intermediate workflow event and execution
terminal. Provider registration validates each set as bounded ASCII labels,
requires global uniqueness across the three dispositions and calls the
Provider classifier for every declaration. A type cannot therefore be selected
as an intermediate inbox in Storage while the same Provider classifies it as a
terminal credential or execution result.

Empty sets preserve Providers whose browser path does not use that result
stage. Unknown types remain outside all Core workers. This declaration layer is
the prerequisite for adding the UAI cursor-result processor: SQL can select
only the exact intermediate and execution-terminal types without allowing an
unknown or credential result to occupy its bounded claim window.

## Two-hundred-and-thirteenth Phase 0 slice

The Provider API now has an owned, consuming BrowserBridge workflow-result
boundary. Core supplies the exact issued exchange, decrypted command, optional
encrypted workflow plan, optional sequence-bound runtime sidecar, raw result,
runtime origin/frame binding and frozen runtime settings. Generic validation
checks every type, size, digest, session, sequence and timestamp before any
Provider-specific plan, cursor or DOM-result parser runs; Debug output redacts
all artifact bytes and the remote Task identity.

An intermediate callback can return only the exact completed exchange plus one
contiguous next issued command and its optional exact runtime sidecar. An
execution-terminal callback can return only the exact completed exchange plus
a fresh `RemoteProgress` observed no earlier than the terminal result. Both
constructors compare the completed command identity with the original issued
exchange, not only with result metadata, so a Provider cannot substitute a
different command at the same session and sequence.

This contract grants no persistence or dispatch authority by itself. The next
Core slice must atomically complete the current exchange while either storing
the next encrypted command/state or terminating the session with independently
verified progress; only after that transaction exists can the daemon invoke
UAI's persisted cursor-result adapter.

## Two-hundred-and-fourteenth Phase 0 slice

Storage now consumes a claimed BrowserBridge workflow result through one
immediate transaction. It rechecks the Provider-scoped owner, account and Task
binding, the still-issued command identity, the exact stored result metadata,
the live worker lease and the session runtime binding before changing durable
state. An intermediate result completes the current exchange and stores the
contiguous next command plus optional runtime sidecar as encrypted secret blobs
in that same transaction. A terminal result completes the exchange, records
the independently verified progress and completes the helper session while
revoking both helper tokens.

Migration 051 adds `processed_at` as an explicit successful consumption marker.
Pending, retry, expired-lease and attempt-budget queries exclude processed
results, preventing a successful final-budget callback from later being
recovered as dead letter. The worker claim is cleared only inside the successful
transition transaction; a wrong or expired claim leaves the exchange, next
command and session untouched.

## Two-hundred-and-fifteenth Phase 0 slice

The first command of a multi-step BrowserBridge workflow can now freeze a
session-level recovery context in the same transaction as command issuance.
The context contains canonical serialized runtime settings plus an optional
bounded Provider-private plan. Settings are digest-bound; plan bytes are stored
as an encrypted secret with an exact type and digest. A duplicate first issue
must reproduce the same command, runtime sidecar and workflow-context metadata
or it is a sequence conflict.

Every later command recovery resolves the immutable context through the same
owner/account/Task/Provider-scoped secret boundary. Changed settings JSON,
changed plan ciphertext, missing plan metadata or a foreign secret owner fails
closed. This gives a daemon result processor the exact creation-time settings
and Provider plan after restart instead of re-resolving mutable settings or
asking the helper to echo workflow authority.

## Two-hundred-and-sixteenth Phase 0 slice

Provider runtime schemas can now declare one portable
`minimum_answer_coverage` Core behavior. Core resolves it as integer
thousandths, with `1000` as the conservative default when no Provider declares
the behavior. A declared setting must be a bounded `DecimalMillis` value from
1 through 1000 and may be overridden only through its explicit Master-owned
Provider, account or Task scopes.

This setting is policy input, not permission to invent answers or silently
drop Questions. The following Draft slice must calculate it against the full
immutable Question snapshot and persist the selected/unanswered partition plus
the resolved threshold before a subset can become executable.

## Two-hundred-and-seventeenth Phase 0 slice

Engine and `asterismd` now consume declared BrowserBridge intermediate and
execution-terminal result inboxes. Validation reconstructs the encrypted
command, optional runtime sidecar and session-level workflow context, then
rebinds owner, account, Task, Provider version, runtime origin and the frozen
settings schema before invoking the Provider callback. Missing context,
undeclared result types and changed plan digests fail before Provider code can
return a transition.

The daemon starts this worker only for Providers with non-empty workflow result
sets. Each Provider has a separate repository scope and worker identity; each
tick claims at most one result. A validated transition enters Storage's atomic
commit boundary, while recovery/validation/storage failures use the existing
bounded retry and dead-letter ledger. Binding, sequence or claim conflicts are
terminal and never dispatch or replay a browser command.

## Two-hundred-and-eighteenth Phase 0 slice

An immutable `SubmissionDraft` now proves answer coverage against its complete
fresh Question snapshot. It stores the total Question count, the resolved
fixed-point minimum and the exact unanswered Question IDs. Domain validation
requires selected and unanswered IDs to be a disjoint complete partition and
uses integer arithmetic for the threshold. Unsupported or unresolved Questions
therefore remain in the denominator without receiving invented answers.

Submission build resolves the Provider default, account and Task settings and
passes only selected Questions to the Provider's credential-free preview. A
Provider without the portable coverage behavior remains at 100%; Chaoxing's
audited schema declares 900 by default. Migration 053 persists the frozen
partition. Storage compares it with the real snapshot Question IDs on both
write and read, so a changed count or foreign unanswered ID fails closed before
the Draft can be scheduled or recovered.

## Two-hundred-and-nineteenth Phase 0 slice

Answer Evidence now has a durable two-layer Core boundary. A private evidence
record retains its exact owner, Provider account, Course, Task, immutable
Question snapshot, candidate, execution Attempt, verification digest and
sanitized provenance. Storage rechecks those bindings against the original
Question, candidate and Attempt in one immediate transaction; a foreign owner,
account, Task, Question or Attempt leaves no private or global partial write.

Safely representable evidence is projected into an identity-free global Corpus
entry containing only the exact Question content fingerprint, sanitized
semantic Question asset and semantic answer. Snapshot-local option IDs are
rebound to option content, and choice selections are canonicalized in Question
order. Official, verified historical and negative evidence have independent
counts. A stable source-fact digest makes retries idempotent without treating a
second owner's corroborating evidence as a duplicate. Unsupported compound or
context-dependent shapes remain durable explicit unmatched evidence instead of
being guessed or discarded. Migration 054 persists both layers and their exact
projection link; the public Corpus read model exposes no owner, QQ, account,
Course, Task, snapshot, candidate, Attempt or private provenance identity.

## Two-hundred-and-twentieth Phase 0 slice

The first verified transition of a Provider account into `Authenticated` now
atomically creates one durable generation-1 read-only Answer Bootstrap Harvest
and its scheduler job. The trigger is centralized in the credential transaction
shared by normal credential commits, authentication Bootstrap, BrowserBridge
credential capture and runtime renewal, so a successful account transition
cannot commit without its initial harvest record. Merely creating an idle
account or storing an unverified credential does not claim authentication and
does not start a harvest.

The harvest persists owner, Provider/account binding, generation, scheduler
identity, lifecycle state, bounded progress, sanitized watermark and recovery
timestamps. Its owner-scoped read model verifies the exact scheduler payload
before returning it. The `(provider_account_id, generation)` uniqueness and
stable scheduler idempotency key ensure Cookie rotation, renewal, repair and
later reauthentication cannot create another default full scan. Explicit
future rescans must use a new generation and a separately authorized path.
Migration 055 adds the durable ledger; this slice creates only the read-only
job authority and does not enumerate history, create remote Attempts or mutate
Provider Tasks.

## Two-hundred-and-twenty-first Phase 0 slice

Answer Bootstrap Harvest jobs now have an atomic, bounded worker lifecycle.
Claiming a due job binds one worker lease and moves its exact harvest from
pending or paused into running in the same immediate transaction. An expired
lease can be reclaimed without losing the persisted watermark; repeated lease
loss consumes the same five-attempt budget as explicit retry failures, and an
exhausted job becomes a failed harvest plus scheduler dead letter instead of
looping indefinitely.

Every checkpoint rechecks harvest ID, scheduler ID, worker identity and a live
lease before accepting monotonic scanned/total progress and a sanitized
watermark. Retryable failure atomically pauses the harvest and reschedules its
job; terminal failure closes both records. Successful completion requires the
final scanned count to equal the declared total, then completes the harvest and
scheduler job together. Wrong workers, expired leases, regressing progress,
changed totals and malformed errors leave both records unchanged. This remains
a protocol-neutral read-only lifecycle: Provider history enumeration and
evidence extraction adapters are still separate capability work.

## Two-hundred-and-twenty-second Phase 0 slice

Private Answer Evidence can now bind historical verification to an exact
authenticated Provider Attempt digest without requiring a local
`ExecutionAttemptId`. Verified historical and negative evidence still require
a bound source candidate and non-zero result digest; they now require at least
one real Attempt provenance path: a local execution Attempt, a Provider-native
historical Attempt digest, or both. Zero digests remain invalid.

Migration 056 persists the Provider Attempt digest inside the owner-private
evidence layer, and the stable evidence-fact digest includes it for idempotent
replay. It is never copied into the identity-free Global Corpus. This removes
the incentive to fabricate an Asterism Execution merely to import read-only
history and gives the upcoming Provider harvest protocol a valid provenance
target while preserving the bootstrap rule that collection creates no remote
or local task Attempt.

## Two-hundred-and-twenty-third Phase 0 slice

Provider API now defines a credential-free, read-only Answer History Harvest
contract. A Provider enumerates bounded pages of safely readable historical
Task/Attempt references using a versioned, Provider-prefixed, sanitized cursor;
each reference carries a non-zero Provider Attempt digest and ephemeral
non-serialized route context. Empty intermediate pages, duplicate Attempt
identity, cross-Provider cursors, secret-shaped metadata and ambiguous terminal
cursors fail closed.

After Core maps a reference to its owner-private local Task, the Provider may
return one exact result snapshot: bounded normalized Questions, submitted and
official answers, per-Question correctness, fixed-point score, retake facts,
result digest, Provider Attempt digest and sanitized provenance. Question,
answer and Task bindings are validated before ingestion. Diagnostics expose
only counts and digest markers, not stems or answers. The contract grants no
submission, unlock or retake method; capability advertisement/registry wiring
and the first evidenced Provider adapter remain the next slices.

## Two-hundred-and-twenty-fourth Phase 0 slice

The Provider registry now owns an explicit `AnswerHistoryHarvest` capability
slot. Registration requires exact agreement between metadata advertisement and
the trait implementation, and the implementation must return the same complete
Provider metadata identity as every other slot. A metadata-only declaration,
an unadvertised implementation or an implementation borrowed from another
Provider fails registration.

All existing Provider and test entries were migrated explicitly with no
history slot, so unsupported platforms cannot accidentally turn the default
bootstrap job into a false empty success. The generic scheduler also cannot
claim these jobs outside their atomic harvest lifecycle. The next Provider
slice may advertise the capability only where audited result/history protocol
evidence can produce bounded pages and exact Task evidence.

## Two-hundred-and-twenty-fifth Phase 0 slice

Historical result ingestion now has one atomic Storage boundary for the fresh
Question snapshot, answer candidates, owner-private evidence, identity-free
Corpus projections and a durable import ledger. A history worker therefore
cannot persist half of a result when a late candidate/evidence binding check or
database write fails. The bootstrap harvest remains read-only: this boundary
records evidence already obtained from the Provider and creates no local or
remote execution Attempt.

Each ledger row is uniquely bound to Provider account, local Task, Provider
Attempt digest and result digest. It also freezes a canonical semantic content
digest that deliberately excludes newly generated local UUIDs. Replaying the
same remote result after a process failure returns the original import record
without adding snapshots, candidates, private evidence or global corroboration
counts; reusing the same remote identity with changed parsed Questions,
answers, classifications or provenance fails closed as protocol drift.

Harvest progress is checkpointed only after this transaction succeeds. If the
process commits an import and crashes before advancing its watermark, the next
claim can safely reread and reingest that exact Provider result, observe the
ledger duplicate and then advance. Migration 057 persists this crash-recovery
boundary without treating a duplicate import as new Answer Evidence.

## Two-hundred-and-twenty-sixth Phase 0 slice

The bootstrap Answer History worker now has a bounded one-Provider-page claim
boundary. It decodes only a versioned, Provider-bound cursor, resolves each
remote Task identity inside the exact Provider account, validates the returned
Attempt/result evidence, imports it through the atomic history ledger and only
then advances progress. A successful non-terminal page atomically persists the
next cursor and releases the scheduler claim without consuming a retry attempt;
the next due claim resumes from that cursor. A terminal page completes with
`total == scanned`. Repeated non-terminal cursors, cross-account Tasks, future
observations and changed result bindings fail closed.

The owner-private history layer also retains submitted answers whose result page
does not expose a reliable per-Question verdict. Migration 058 permits a
history import to contain candidates with zero reliable evidence: those
candidates remain attached to the immutable private snapshot and import ledger,
but do not increment or project into the identity-free Global Corpus. Official
answers become Official evidence; submitted answers become VerifiedHistorical
or Negative only when the Provider supplies an exact positive or negative
verdict. Score, retake facts, result surface and task/question provenance remain
private evidence provenance.

The Engine deliberately is not attached to the daemon loop yet. Bootstrap jobs
must not be claimed merely because an account exists: daemon activation waits
for a real registered Native HTTP, BrowserBridge or Capture-backed history
transport. The synthetic Chaoxing adapter verifies the contract but its native
registry slot remains absent, so existing first-auth jobs cannot be exhausted
by a false unsupported scan.

## Two-hundred-and-twenty-seventh Phase 0 slice

Answer Bootstrap Harvest claiming is now explicitly filtered by the set of
Providers whose current registry entries advertise and implement
`AnswerHistoryHarvest`. Storage applies that filter both to due-job selection
and to expired-claim retirement. An empty set or a set containing only other
Providers returns no claims and leaves the harvest state, scheduler state and
attempt budget untouched.

The Engine derives the eligible set from the already integrity-checked Provider
registry on every tick and passes it into the atomic claim boundary. This makes
daemon activation safe before every Provider has a real transport: unsupported
accounts retain their one durable first-auth job for a future implementation,
while a Provider becomes executable only when metadata and the exact history
capability slot register together.

## Two-hundred-and-twenty-eighth Phase 0 slice

`asterismd` now owns the Answer History worker lifecycle alongside scan,
execution, BrowserBridge and outbox workers. It reuses the scheduler enable
switch, tick interval, claim lifetime and bounded retry delays, participates in
the same watch-channel shutdown, and emits logs only for non-empty tick reports
or failures. The history batch is capped at Core's 100-claim boundary even when
the shared scheduler permits a larger scan batch.

The daemon starts this worker only when the integrity-checked registry contains
at least one advertised and implemented `AnswerHistoryHarvest` slot. With the
current native Provider entries the worker is therefore absent, not polling in
an idle loop. Once a real Native HTTP, BrowserBridge or Capture-backed adapter
is registered, the same daemon path becomes active and Storage's Provider
filter ensures it claims only that adapter's durable first-auth harvests.

## Two-hundred-and-twenty-ninth Phase 0 slice

Normal task execution now incrementally grows the Answer Evidence Corpus at
the `SubmissionVerify` persistence boundary. Saving a durable
`SubmissionResult` and projecting its reliable per-Question facts execute in
the same immediate transaction. The evidence binds the exact owner, Provider
account, Course, Task, immutable Question snapshot, selected candidate, local
Execution Attempt and a stable verification digest; any binding or Corpus
failure rolls back the result and every projection together. The unique result
per Execution Attempt is the incremental ingestion ledger.

Only explicit per-Question verdicts are evidence. A Confirmed selected answer
becomes VerifiedHistorical; a Rejected selected answer becomes Negative;
Unverified and provider skip commands create no Answer Evidence. Overall task
success or score never invents missing per-Question correctness, and
`SubmissionVerify` exposes no official answer so this path never labels a
selection Official. Result status, remote state, score, progress and sanitized
receipt remain in owner-private provenance.

Corpus projection eligibility is now one Domain rule shared by bootstrap and
incremental ingestion. Exact choice/text/boolean facts project only when their
semantic shape is identity-free; attachments, unsupported hierarchies and
incomplete option content remain durable unmatched private evidence without
polluting global answer counts.

## Two-hundred-and-thirtieth Phase 0 slice

Strict Completion and Score Improvement now exist as separate Domain state
machines with separate typed identities. Both freeze one bounded policy
snapshot containing enable switches, attempt limits, deadlines, target score
and formal-retry confirmation behavior. The default enables both policy
switches, keeps actual Score Improvement execution explicitly opt-in, caps both
flows and requires confirmation for formal assessment retries.

Strict Completion starts active only when enabled, records structured
`CompletionDiagnosis` values, and becomes terminal immediately after a fresh
Completed or Passed observation. Mechanical insufficiency may return to active
within bounds; prerequisites, teacher/human action, unsupported behavior,
protocol drift, unknown remote state, closed windows and exhausted attempts
stop automatic work. A second formal submission attempt cannot start without
explicit confirmation.

Score Improvement can start only from an immutable verified completion
baseline, with both policy enablement and explicit opt-in. Unknown or
teacher-defined retake score policy stops for human action before mutation.
Every retake requires confirmation, preserves the original completion outcome,
tracks the best observed score by normalized ratio and terminates at target,
remote/attempt limits or diagnosis. A lower result cannot turn the completed
baseline back into an incomplete Task.

## Two-hundred-and-thirty-first Phase 0 slice

Migration 059 persists Strict Completion and Score Improvement in separate
tables with separate IDs, one owner/account/Task binding each, bounded state
columns, the complete immutable workflow JSON and an optimistic revision. The
Storage create boundary rechecks the real Task -> ProviderAccount -> owner
relationship and is exactly idempotent; another workflow for the same Task or
reused identity conflicts instead of replacing policy or history.

Storage exposes operation-shaped transitions rather than a generic row update.
Under one immediate transaction it loads and validates the persisted Domain
state, verifies owner and expected revision, invokes the corresponding Domain
`begin` or `observe` transition, validates the successor, and advances revision
with compare-and-swap. Missing confirmation, stale callers, illegal terminal
restarts, regressing time and malformed observations leave the durable workflow
unchanged.

Owner-scoped reads decode and cross-check every duplicated identity, state and
timestamp column against the workflow document. Score Improvement persistence
therefore cannot overwrite the immutable Completed/Passed baseline even when a
retake score is lower, and Strict Completion cannot be revived after a verified
terminal outcome.

## Two-hundred-and-thirty-second Phase 0 slice

Completion policy is now part of every registered Provider runtime-settings
schema through seven canonical Core-owned definitions. Provider schemas cannot
claim the reserved keys or duplicate their Core behaviors. Resolution keeps the
existing Provider -> ProviderAccount -> Task precedence and source attribution,
then freezes enablement, attempt limits, score target and wall-clock bounds for
the Execution. Strict Completion defaults on; post-completion Score Improvement
remains explicit opt-in; formal assessment retry confirmation is deliberately
not configurable.

Registry injection also preserves already durable executions. Before recovery
or Provider dispatch, Core may hydrate only missing canonical completion fields
from their fixed defaults into an in-memory copy of a legacy frozen snapshot.
The exact complete-snapshot validator remains strict, and missing
Provider-authored fields, unknown keys, invalid values or schema-version drift
still stop execution. BrowserBridge recovery follows the same boundary, so an
upgrade neither strands pre-existing work nor silently broadens its Provider
settings.

## Two-hundred-and-thirty-third Phase 0 slice

Every newly scheduled Execution now derives a typed `CompletionPolicySnapshot`
from the fully resolved Provider -> ProviderAccount -> Task settings hierarchy
and persists it atomically beside the resolved values, source map and layer
revisions. Storage validates the policy bounds and exact capture-time binding,
re-resolves all current layers inside the scheduling transaction, derives the
same policy again and rejects the whole request if either settings or policy
changed before commit.

Migration 060 rebuilds `execution_runtime_settings` with a non-null completion
policy document. Existing rows receive the canonical Strict Completion and
Score Improvement defaults with absolute seven-day and one-day deadlines based
on their original `captured_at`; the migration preserves the timestamp's
nanosecond suffix rather than accepting SQLite's millisecond formatting loss.
The migration has a populated-row regression, and API coverage proves a Task
attempt-limit override is frozen into the durable policy rather than merely
remaining in the generic settings map.

## Two-hundred-and-thirty-fourth Phase 0 slice

Provider execution and submission-verification contracts now expose optional,
typed completion-diagnosis hooks without putting Provider wire details into
Core. Providers may map audited result facts to reasons such as insufficient
duration or pending required children; the default returns no diagnosis and
therefore grants no automatic retry authority.

The Engine has one pure observation boundary for both execution and submission
results. It validates the Provider payload, recognizes completion only from
fresh verified `Completed`, ignores any conflicting diagnosis after completion,
maps an otherwise rejected submission to `ScoreBelowThreshold`, and maps every
other unsupported incomplete result to `RemoteUnknown`. Each observation thus
contains exactly one completion outcome or one diagnosis and is ready for a
later atomic workflow transition without treating mutation success as Task
completion.

## Two-hundred-and-thirty-fifth Phase 0 slice

Migration 061 adds an exactly-once Strict Completion observation ledger keyed
by both Execution and ExecutionAttempt. The Storage operation accepts only the
worker's live scheduler claim and lease, derives owner/account/Task/formal
assessment and frozen completion policy from the durable execution graph, and
atomically creates or advances the Task's Strict workflow together with the
observation row. Exact replay returns the existing record; a conflicting
attempt or outcome fails without incrementing the workflow attempt count.

Strict Completion is now monotonic across manual or delayed resolution. A
diagnosed attempt can remain Active for another bounded attempt or stop at a
hard/attempt limit, while any later fresh Completed/Passed fact can close an
Active, Disabled or Stopped workflow as Completed. It cannot replace one
verified completion outcome with another or restore automatic work from a
diagnosis. Storage coverage exercises replay plus three separate Executions:
retryable diagnosis, attempt-limit stop, then verified completion. The runner
call sites are intentionally the next slice; this slice establishes their
atomic crash-recovery boundary first.

## Two-hundred-and-thirty-sixth Phase 0 slice

The execution worker now writes final reliable completion observations through
the atomic Strict workflow ledger before it closes the worker-owned Execution.
This covers ordinary single-capability results which satisfy their verified
execution goal, fresh goal verification without replaying mutation, terminal
SubmissionVerify results, and terminal submission readback after verification
recovery is exhausted. The existing SQLite execution repository delegates the
completion aggregate on the same Database, so daemon construction and worker
concurrency ownership do not gain a second mutable connection or a parallel
state owner.

Execution success remains separate from Task completion. A verified
DurationReport may finish its requested operation while a Pending/InProgress or
Unknown completion observation stops Strict Completion as `RemoteUnknown` when
the Provider has no stronger audited diagnosis. A fresh generic Completed or
Submission Confirmed observation instead creates a Completed workflow and
ledger row. Runner integration tests assert all three cases against durable
rows. Composite execution and generic execution-recovery final observations
remain explicit next slices; intermediate results which are still entering
verification recovery are deliberately not recorded as final observations.

## Two-hundred-and-thirty-seventh Phase 0 slice

Composite execution now records exactly one Strict Completion observation from
the final Provider call's verified outcome. Earlier calls still durably finish
their own capability steps but cannot consume the Execution-level observation
key or stop the workflow from an intermediate DurationReport. Both ordinary
multi-call plans and one-call atomic composite plans therefore converge through
the same final ledger boundary.

Generic recovery now resolves the exact unfinished ExecutionAttempt before a
Provider verification read. A final verified outcome is ledgered before
recovery closes; a reliable incomplete observation is ledgered only when the
recovery retry budget is exhausted. Recovery which will retry writes no final
observation. Providers without goal-bound ExecutionVerify may instead recover
through fresh TaskProgress: Completed is recorded as a monotonic completion,
while Pending/InProgress which schedules another retry remains unrecorded.
Tests cover final composite uniqueness, recovered completion and the absence of
an observation on a pending retry.

## Two-hundred-and-thirty-eighth Phase 0 slice

Migration 062 corrects the Strict Completion observation ledger cardinality for
Core's existing Retry model. A Retry reuses one Execution but creates a new
ExecutionAttempt, so exactly-once identity now uses `execution_attempt_id` as
the primary key while retaining a non-unique indexed Execution binding. The
same attempt cannot be counted twice, but one Execution may accumulate bounded
observations across distinct retry attempts.

The migration rebuilds and copies the populated migration-061 table before
recreating workflow and Execution indexes. Its upgrade regression preserves an
existing first-attempt diagnosis and then inserts a Completed observation for a
second attempt of the same Execution. Storage lookup now resolves idempotency by
attempt first and separately verifies the Execution binding, preventing a
foreign attempt from reusing another Execution's ledger entry.

## Two-hundred-and-thirty-ninth Phase 0 slice

The execution worker now consumes the atomic Strict Completion observation
record instead of discarding the updated workflow state. When a reliable
non-Submission result leaves a routine Task's workflow Active, Core closes the
current ExecutionAttempt as failed, moves the same Execution to RetryWaiting
and creates the existing numbered Retry job. The Retry starts a fresh
ExecutionAttempt, so every additional observation retains its own attempt
provenance while the frozen completion policy remains unchanged.

Continuation is bounded independently by both the Strict workflow and the
worker retry policy. A terminal Completed/Passed or stopped/disabled workflow
does not retry; an exhausted worker retry budget or a next run time at/after
the frozen Strict deadline requires human action. Formal assessment never
receives an automatic second attempt merely because execution was authorized:
an Active first result becomes HumanRequired until the separate confirmation
flow grants a retry. Submission is deliberately excluded because its next
attempt must build a fresh QuestionSnapshot, Draft and Attempt rather than
replaying the prior immutable Draft.

Engine coverage runs one incomplete DurationReport through three distinct
attempts of the same Execution, observes two numbered retries, then proves the
third observation stops at the frozen attempt limit with three unique ledger
bindings. A separate formal-assessment regression enables execution, records
the first incomplete observation and proves that no Retry job is created.

## Two-hundred-and-fortieth Phase 0 slice

Owners can now read both completion state machines through one Task-bound,
read-only API resource. The response contains the Strict Completion workflow
and Score Improvement workflow independently, including each optimistic
revision and complete frozen Domain snapshot. A workflow which has never been
created is returned as null; the read does not create, restart or otherwise
advance either state machine.

The route first proves the Task belongs to the authenticated reader, then uses
the owner-and-Task scoped completion repository for both lookups and returns a
`no-store` response. Invalid or foreign Task identities therefore cannot be
used to probe whether a workflow exists. The OpenAPI client contract describes
the complete binding, policy, state, diagnosis, completion baseline and score
shapes rather than collapsing the two workflows into one completion flag.

API integration coverage reads an existing Task before a workflow exists,
persists one Strict workflow through the real Storage boundary, and then reads
back its owner binding, Active state and revision while Score Improvement
remains independently absent. This observable revision boundary is required
before later confirmation and fresh Snapshot/Draft retry commands can safely
use compare-and-swap semantics.

## Two-hundred-and-forty-first Phase 0 slice

Migration 063 adds a separate durable Formal Strict Completion retry
confirmation. Each record binds one Execution to the exact Strict workflow ID
and expected revision, confirming owner and timestamp. Confirmation is not an
Execution request boolean and cannot be supplied by a worker at observation
time. Idempotent scheduling compares the persisted confirmation as part of the
original request identity.

The scheduling transaction now reads the current owner/Task workflow before it
moves the Task or creates an Execution. A Formal workflow which is Active after
at least one attempt requires an exact current-revision confirmation; missing,
foreign, stale, terminal or unnecessary confirmations roll back as a typed
conflict. This confirmed path is the sole exception which lets an owner move a
HumanRequired Task back to Scheduled. The worker later derives retry authority
from the stored Execution confirmation and verifies the workflow ID/revision
again before advancing the attempt counter.

Formal Submission retry also proves freshness at the transaction boundary.
Both the immutable SubmissionDraft and its QuestionSnapshot must be newer than
the workflow's previous observation, and the existing one-Draft/one-Execution
constraint remains in force. Storage tests reject a reused/late-built Draft
over an old Snapshot, accept a fully fresh pair, and carry the persisted
confirmation through a live claimed Attempt into workflow attempt two. API
coverage rejects a stale revision, accepts the exact confirmation and returns
the original Execution on an exact idempotent replay.

## Two-hundred-and-forty-second Phase 0 slice

Submission terminal handling now consumes the atomic Strict Completion
workflow result instead of treating the Provider result status as an
independent completion decision. A fresh remote Completed/Passed observation
therefore finishes the Execution successfully even when weaker status fields
conflict, preserving monotonic completion. A rejected or below-threshold
Submission leaves the workflow Active and moves both Execution and Task to
HumanRequired without scheduling an automatic retry or replaying the old
Draft. Terminal states outside those two cases retain their conservative
status mapping.

The same rule applies during recovery. When a SubmissionResult already exists,
the recovery worker first records its exactly-once completion observation and
then finishes from the resulting workflow state; it cannot bypass the ledger
because a crash happened between result persistence and Execution finish.

Retry confirmation now covers every already-attempted Active
SubmissionExecute workflow, regardless of whether the Task is classified as
Formal or Routine. Storage still requires a current workflow revision and a
QuestionSnapshot plus immutable Draft created after the prior observation.
Formal non-Submission Strict retries retain the same confirmation boundary,
while routine idempotent completion work remains eligible for its bounded
automatic retry policy.

## Two-hundred-and-forty-third Phase 0 slice

Tasks now expose a paginated, owner-scoped Attempt History index. Each entry
groups one Execution with all of its durable ExecutionAttempts and, when it is
a submission, includes immutable Draft/QuestionSnapshot references,
answer-source counts and the latest SubmissionResult reference. The result
summary preserves status, score, remote state, progress, verified time and
confirmed/rejected/unverified Question counts without duplicating the full
Question payload; clients use the existing bound Draft and Result resources
for complete selected-answer and readback detail.

Every listed ExecutionAttempt also reports counts of Official,
VerifiedHistorical and Negative private evidence learned by that exact
attempt. Storage proves the attempt belongs to the requesting owner before
counting. Scored results include the previous scored result on the same owner
and Task and a normalized `-1000..1000` score delta, so differing raw score
denominators cannot produce a false comparison. The first scored result has no
previous score or delta.

The endpoint performs no Provider call, is `no-store`, validates bounded
pagination and returns 404 for missing or foreign Tasks. OpenAPI describes the
complete index and its references. Integration coverage traverses a real
persisted Candidate, Draft, ExecutionAttempt, SubmissionResult and automatic
VerifiedHistorical projection through the API while preserving owner scope.

## Two-hundred-and-forty-fourth Phase 0 slice

Core now defines a validated `CourseAggregateProgress` read model and exposes
it at the owner-scoped Course boundary. The aggregate separates total
discovered Tasks from the non-Removed completion denominator and reports fresh
persisted counts for remote Completed, remaining, NotOpen, CreditBlocked,
HumanRequired and Failed facts. Diagnostic counts intentionally remain
independent rather than forcing overlapping platform states into a false
partition.

Completion is a `0..1000` ratio and exists only with a non-zero denominator.
Score aggregation selects the latest persisted SubmissionResult per countable
Task, validates its verification snapshot, normalizes each score to the same
thousand-point scale and returns the average plus latest verification time.
Older results on the same Task do not double-count. Required-task and duration
dimensions have explicit Domain types but remain null until audited upstream
facts can populate them; absence is not converted to zero.

The Storage query first proves Course ownership through ProviderAccount and the
API returns `no-store`, 400 for malformed IDs and 404 for missing or foreign
Courses. Storage coverage verifies Removed exclusion, completion and diagnostic
counts, newest-score selection and owner isolation. API coverage reads the
Course and Task created by a real scan and confirms unknown dimensions remain
null. OpenAPI describes the complete response.

## Two-hundred-and-forty-fifth Phase 0 slice

Core now exposes a typed Account Health read model for each owner-scoped
ProviderAccount. Authenticated maps to Healthy; active credential validation is
Checking; Refreshing is ExpiringSoon; expired, human-action, failed/provider-
unavailable and client-update states remain independently visible as Expired,
HumanActionRequired, Broken and ProtocolChanged. Idle and cancelled accounts
report AuthRequired rather than pretending to be healthy.

Storage also searches the account's Task Executions for the newest
attempt-bound `protocol_drift` error. It overrides the authentication-derived
health only when the drift timestamp is not older than the account's latest
state update, so a later repair or reauthentication clears stale protocol
diagnosis. The response retains the exact Execution ID and observation time;
ordinary failures cannot trigger this state.

The endpoint is `no-store`, requires account-read authority and returns 404 for
missing or foreign accounts. Domain coverage fixes the precedence rule,
Storage coverage proves live Attempt provenance and owner isolation, and API
coverage verifies a new Idle account reports typed AuthRequired. OpenAPI
describes every health and reason value. This slice is observation only; the
audited stored-session, renewal, BrowserBridge/Capture and HumanRequired
fallback chain remains a later execution workflow.

## Two-hundred-and-forty-sixth Phase 0 slice

Migration 064 adds the Protocol Observation Inbox as an aggregate table plus a
separate occurrence ledger. The aggregate is unique by Provider, protocol
surface, drift kind and SHA-256 shape digest. Each occurrence has its own
non-zero digest, optional Execution and observation time; replaying that digest
returns the existing aggregate without incrementing its count, while a distinct
occurrence updates first/last seen times and count in one immediate transaction.

Domain accepts only JSON shapes at most 64 KiB, binds the stored digest to the
canonical serialized shape and rejects secret-shaped keys such as Cookie,
authorization, password and access/refresh/client/session secrets. Storage
rechecks the bound. When an occurrence names an Execution, Storage proves its
Task's ProviderAccount has the same Provider ID before any write. A mismatched
or missing Execution, unsafe shape or malformed digest leaves both tables
unchanged.

Masters can list the instance inbox through a bounded `no-store` API filtered
by Provider and drift kind. The response exposes only the sanitized shape and
typed aggregate provenance; OpenAPI includes all surfaces and kinds. Storage
coverage proves aggregation, replay safety, Provider/Execution binding and
secret rejection, while API coverage proves filtered reads and typed query
failure. Provider and Engine drift branches are not silently considered wired:
each still needs an explicit fail-closed emission call in later slices.

## Two-hundred-and-forty-seventh Phase 0 slice

`ProviderError` may now carry one optional boxed `ProviderProtocolObservation`.
The Provider must name the shared protocol surface and drift kind and supply a
shape-only JSON value; the Provider API applies the same 64 KiB and sensitive-
key validation as Domain before accepting the payload. Legacy errors remain
wire-compatible and carry no observation. Core never derives a shape from an
error message, response body or Provider code.

Every Execution worker branch that handles a Provider error now records a
present observation before entering retry, verification-only recovery or a
terminal state. Core accepts the payload only on `ProtocolDrift` and
`InvalidResponse`, redigests the shape and binds the occurrence digest to the
Provider, Execution, scheduler job, correlation, error kind, surface and shape.
The daemon always supplies the SQLite inbox repository. Persistence failure is
not ignored: the claimed job fails through the existing storage error path and
may safely replay because the occurrence digest is deterministic.

Coverage proves that an identical bound occurrence is idempotent and a distinct
occurrence aggregates into the same shape. The shared path is now wired; each
Provider must still attach observations only at explicit unknown type/result/
field branches where it can produce a redacted structural fact without guessing.

## Two-hundred-and-forty-eighth Phase 0 slice

The occurrence validator and digest builder are now one Engine-internal path
shared by Execution and account scan calls. Its bounded occurrence scope joins
Provider, optional Execution, Provider error kind, protocol surface, drift kind
and shape digest. This keeps idempotency semantics identical when later Core
services gain observation sinks instead of allowing each worker to invent its
own hash format.

`ProviderScanService` now records a present observation from CourseInventory or
TaskInventory before returning the Provider failure to the scheduler. Scan
observations deliberately carry no Execution ID; their occurrence scope is
bound to the ProviderAccount and validated scan correlation. The daemon injects
the same SQLite inbox repository into both scan and Execution workers. A valid
Provider failure is returned only after observation persistence succeeds; an
invalid payload becomes non-retryable InvalidInventory, while an Inbox storage
failure remains a retryable scan storage failure.

Coverage proves that replaying the same account scan correlation does not
inflate the aggregate, retains the typed task-inventory/unknown-task-type
classification, never invents Execution provenance and still leaves the scan
ingestion batch empty after Provider failure.

## Two-hundred-and-forty-ninth Phase 0 slice

`SubmissionDraftBuildService` now accepts the shared observation repository and
records a Provider-supplied shape when `SubmissionBuild` fails. The occurrence
scope binds Task, immutable QuestionSnapshot and request correlation; it has no
Execution reference because Draft construction precedes an Execution/Attempt.
The API always injects the SQLite inbox repository.

All existing authorization, snapshot/account binding, explicit Candidate
selection and answer-coverage checks still run before the Provider call. A
Provider failure is returned only after its present observation is durable, and
no Draft is persisted. Invalid observation payloads map to the same fail-closed
bad-gateway surface as an invalid submission preview; Inbox storage failure
remains an internal persistence failure rather than being hidden behind the
Provider error.

Coverage uses an unknown numeric Question type and proves the typed
SubmissionBuild/UnknownQuestionKind aggregate is stored without invented
Execution provenance while the Draft repository remains empty.

## Two-hundred-and-fiftieth Phase 0 slice

`ProviderQuestionReadService` now uses the shared observation recorder for
ordinary Question inventory/parsing and every Provider-controlled durable
pre-Question step: attempt preparation, operation preparation and operation
execution. The occurrence scope binds Task, request correlation and the exact
read stage. These owner-triggered reads precede a Task Execution, so the Inbox
reference remains null rather than inventing an Attempt.

Durable ordering preserves mutation safety. A preparation failure is observed
before it is returned because no operation was issued. When an issued operation
fails, Core first persists the operation as Ambiguous, then records the shape,
then returns the Provider failure; neither observation persistence nor recovery
may replay that operation. Invalid observation payloads share the fail-closed
invalid-Questions API surface, while Inbox storage failure remains internal.

Coverage proves a parser-reported unknown Question type enters the typed
QuestionParse/UnknownQuestionKind aggregate and the failed all-or-nothing read
persists no QuestionSnapshot.

## Two-hundred-and-fifty-first Phase 0 slice

Owner-triggered TaskProgress and DurationRead services now inject the same
protocol observation repository as background workers. Present Provider shapes
are recorded before the read error is returned, with a Task-and-correlation
occurrence scope and no invented Execution provenance. Authorization, declared
capability checks and authenticated account binding still precede every
Provider call.

The Provider owns the protocol surface classification. A shared remote document
such as WELearn CMI may therefore classify both normalized progress and duration
read failures as TaskProgress/UnknownResultShape; Core keeps distinct
`task-progress` and `task-duration` occurrence scopes while the Inbox aggregates
the equivalent shape. Invalid payloads use the corresponding invalid-read
bad-gateway response and Inbox storage failures remain internal.

Coverage proves both read paths persist typed result-shape observations without
an Execution reference before returning their original ProtocolDrift or
InvalidResponse error.

## Two-hundred-and-fifty-second Phase 0 slice

Owner-scoped TaskDetail reads now use the shared protocol observation sink.
After ownership, account authentication and capability checks, a
Provider-supplied structural shape is persisted under a Task-and-correlation
occurrence before the original Provider error is returned. The observation has
no Execution reference and cannot cause the returned detail or local Task to be
updated.

The API injects the SQLite repository. Invalid shape payloads share the existing
invalid-detail bad-gateway response, while Inbox storage failure stays an
internal persistence error. Coverage proves a typed TaskDetail/FieldDrift shape
is stored without Execution provenance before the read fails.

## Two-hundred-and-fifty-third Phase 0 slice

Provider-native AnswerResolve now records a present structural observation
before returning a Provider failure. Its occurrence scope binds Task, immutable
QuestionSnapshot and request correlation, and remains outside an Execution.
Authorization, account/snapshot binding, durable QuestionSession resolution and
Provider capability checks all continue to run first.

Observation persistence is deliberately separate from answer persistence. A
failed Provider response can enter the Inbox, but no AnswerCandidate record is
written and the shape can never be interpreted as Official, ProviderNative,
VerifiedHistorical or Unverified answer evidence. Invalid observation payloads
share the existing invalid-candidates bad-gateway response; Inbox storage
failure remains internal.

Coverage proves an AnswerResolve/UnknownResultShape aggregate is stored without
Execution provenance while the candidate batch remains empty.

## Two-hundred-and-fifty-fourth Phase 0 slice

The read-only Answer History bootstrap worker now records Provider-supplied
protocol shapes from both page enumeration and individual historical Task
reads. Page occurrences bind the harvest and current scanned-count position;
Task occurrences bind the harvest and local Task. They carry no Execution ID
because the harvest is explicitly prohibited from creating an Attempt.

The daemon injects the shared SQLite inbox repository. Observation persistence
happens before the Provider failure is classified into the harvest ledger. An
invalid payload remains terminal invalid Provider evidence; Inbox storage
failure follows the existing retryable/terminal storage classification. Neither
case advances the cursor or scanned count.

Coverage proves an unknown historical result shape is retained as
SubmissionVerify/UnknownResultShape while the harvest watermark remains at its
initial value and QuestionSnapshot, Candidate, private evidence, global corpus
and import-ledger tables all remain empty.

## Two-hundred-and-fifty-fifth Phase 0 slice

The authenticated manual account-scan API now injects the same SQLite protocol
observation repository as the scheduled scan worker. Both entry points use the
same `ProviderScanService`, account/correlation occurrence identity, failure
classification and all-or-nothing inventory ingestion boundary. This closes an
API composition omission; it does not add another scan protocol or duplicate
observation path.

## Two-hundred-and-fifty-sixth Phase 0 slice

Protocol observation is now explicitly orthogonal to recovery disposition.
`ProtocolDrift` and `InvalidResponse` remain valid observation-bearing errors;
`HumanRequired` is also accepted only when it retains a typed
`HumanRequiredReason`. This lets a Provider preserve an already-known sanitized
shape after a non-idempotent mutation has begun while still forcing manual
review and prohibiting automatic replay.

An untyped `HumanRequired` observation remains invalid. Coverage proves a typed
manual-intervention error can carry a SubmissionExecute result shape through
the Core recorder and that removing its reason fails closed.

## Two-hundred-and-fifty-seventh Phase 0 slice

Owner-scoped BrowserBridge policy reads now record Provider-supplied protocol
shapes under a Task-and-correlation occurrence before returning the Provider
failure. The API injects the shared SQLite repository. Account authentication,
declared BrowserBridge capability and exact owner/Task/Provider binding still
precede the call.

This service only reads and validates a credential-free `BrowserSessionSpec`.
Observation persistence cannot create a BrowserBridge session, navigate, inject
credentials or widen allowed origins. Invalid observation payloads share the
existing unsafe-policy bad-gateway response; Inbox storage failure remains
internal. Coverage proves BrowserBridge/EndpointVersionDrift is retained while
the browser-session table stays empty.

## Two-hundred-and-fifty-eighth Phase 0 slice

Direct owner credential validation now records a Provider-supplied protocol
shape before returning the authentication failure. The occurrence binds the
ProviderAccount and request correlation and carries no Execution reference.
The API injects the shared SQLite observation repository into the credential
service.

Observation is diagnostic only. The candidate credential is never persisted,
the ProviderAccount does not transition out of Idle, and no successful
authentication fact is created. Invalid observation payloads fail through the
existing inconsistent-authentication bad-gateway boundary, while observation
storage failures remain internal. Coverage proves a typed
Authentication/FieldDrift shape is retained with zero secret blobs and no
account-state mutation.

## Two-hundred-and-fifty-ninth Phase 0 slice

The durable AuthSession flow now records Provider-supplied authentication
shapes from challenge creation, session-bound credential validation, external
OAuth exchange and OAuth replacement validation. Every occurrence binds the
AuthSession, stage and correlation and carries no Task Execution provenance.
The account API injects the shared SQLite observation repository at each route
that can call the Provider.

Existing state ordering remains authoritative. Challenge and credential
failures transition the AuthSession before observation; a claimed OAuth
callback first finishes its one-shot pending record as Failed or Ambiguous and
transitions the AuthSession, then records the shape. Observation failure cannot
reopen or replay the callback. Coverage proves typed challenge, credential and
OAuth exchange drift is retained only after those durable transitions, with a
single OAuth Provider call and zero secret writes.

## Two-hundred-and-sixtieth Phase 0 slice

BrowserBridge credential-result validation now records Provider-supplied
shapes from both terminal-result interpretation and fresh validation of the
derived credential. Occurrences bind the BrowserBridge session, exact exchange
sequence, validation stage and correlation; they carry no Task Execution
reference. The daemon injects the shared SQLite repository into every
Provider-scoped credential processor.

The durable BrowserBridge result and command are recovered and rebound before
either Provider call. A retained shape cannot complete the exchange, produce a
validated credential bundle or reach the atomic credential commit service.
Processor retry and dead-letter handling remains unchanged and never replays a
browser command. Coverage proves both BrowserBridge/UnknownResultShape and
Authentication/FieldDrift are stored while validation returns no committable
output.

## Two-hundred-and-sixty-first Phase 0 slice

BrowserBridge workflow-result validation now records a Provider-supplied shape
under the recovered session, exact exchange sequence, completion stage and
correlation. The Provider-scoped daemon processor injects the shared SQLite
repository. The observation has no Task Execution reference because this layer
interprets an already-durable BrowserBridge result rather than executing the
Task action itself.

All owner, Task, account, Provider version, result disposition, runtime setting,
workflow plan, runtime state and artifact bindings remain prerequisites. Core-
detected frozen-plan corruption stays a Core validation error and is not
misattributed as Provider evidence. Coverage proves a typed
BrowserBridge/UnknownResultShape is retained after exactly one Provider call
while no workflow transition reaches the commit boundary.

## Two-hundred-and-sixty-second Phase 0 slice

Capture-based authentication bootstrap now records a Provider-supplied
credential-validation shape under the bootstrap session and request
correlation. The public bootstrap submission route injects the shared SQLite
repository. The observation has no ProviderAccount or Execution provenance
because an AddAccount candidate has not crossed the atomic account/secret
commit boundary yet.

Provider rejection and drift deliberately leave the bootstrap session Claimed
and its access token usable for another bounded Capture submission. Recording a
shape neither consumes that claim nor creates the candidate ProviderAccount,
secret blob or successful binding fact. Coverage proves an
Authentication/FieldDrift observation is durable while the claimed state is
preserved and both account and secret tables remain empty.

## Two-hundred-and-sixty-third Phase 0 slice

The initial Answer History harvest invariant now covers both new bindings and
upgrades. Migration 065 backfills generation 1 only for already-Authenticated
ProviderAccounts that have no initial harvest, preserving the account's owner,
Provider, account identity and last authentication timestamp in one typed
pending scheduler job. Idle and other unbound states are not scheduled.

Normal direct credential, AuthSession, Capture bootstrap and BrowserBridge
credential commits continue to create the same job inside their existing
account/secret transaction when they perform the first transition to
Authenticated. Subsequent credential replacement and replay cannot create a
second full scan. Coverage decodes the migrated scheduler payload, proves the
backfill is idempotent, and verifies direct, AuthSession and BrowserBridge entry
points retain exactly one harvest/job across refresh or duplicate submission.

## Two-hundred-and-sixty-fourth Phase 0 slice

The canonical completion policy and Core-owned runtime setting schema now
default both Strict Completion and Score Improvement policy switches to
enabled. This is permission to create the corresponding bounded workflow, not
automatic score chasing: Score Improvement still requires a separate explicit
owner opt-in after a verified Completed or Passed baseline, and every retake
still requires its existing confirmation and Provider limits.

Explicit Provider, ProviderAccount or Task setting values continue to override
the default, including an explicit false value. Already-frozen Execution policy
snapshots are not rewritten; migration 060's historical backfill remains an
immutable record of the policy frozen at that earlier revision. Coverage proves
the new canonical policy enables both switches while a non-opted-in Score
Improvement workflow remains Disabled.

## Two-hundred-and-sixty-fifth Phase 0 slice

Answer History ingestion now retains owner-private Task-level score, typed
retake facts, sanitized result provenance and Provider observation time in the
same atomic import ledger as the immutable Question snapshot and answer
evidence. All four fields participate in the semantic content digest, so replay
with changed retake eligibility, remaining attempts, score or provenance fails
closed instead of being accepted as the same result.

Migration 066 preserves existing imports with explicit unknown score/retake,
empty sanitized provenance and observation time equal to their prior import
time. The owner-and-Task query returns only the latest exact fact and revalidates
score, retake metadata, digests and timestamps. A historical result with no
available answer candidate may now persist its snapshot and Task facts with
zero candidates/evidence; it still contributes nothing to the Global Corpus.
This read model is the prerequisite for a safe Score Improvement opt-in path and
does not itself create a retake workflow.

## Two-hundred-and-sixty-sixth Phase 0 slice

An owner with Task execution authority can now explicitly opt into Score
Improvement through a dedicated Core/API path after a verified Completed or
Passed baseline. Core requires the latest same-owner/account/Task Answer History
fact to be no older than completion, freezes its import identity and result
digest, and copies only typed score/retake facts into the workflow. Remaining
Provider attempts and result close time can only reduce the frozen Core bounds.

The opt-in is idempotent per Task and performs no Provider call, Execution
creation or remote Attempt start. Unknown/teacher-controlled score-retention
policy remains stopped for human action. A legacy workflow without exact result
authority remains readable but `begin_retake` fails closed. HTTP/OpenAPI
coverage proves explicit `true` is required, one workflow is created, the exact
result authority is returned and a replay returns the same record.

The WELearn one-time full upstream sweep is also consolidated in
`research/providers/welearn/FULL_UPSTREAM_SWEEP.md`. A 2026-08-15 head, tag,
Release and issue refresh found no donor revision or protocol delta. All four
first-batch sweep records required by `NEXT_CHECKPOINT.md` are now explicit;
shared Core gaps and live-validation gates remain active work.

## Two-hundred-and-sixty-seventh Phase 0 slice

Provider-native QR authentication now has a restart-safe Core boundary instead
of process-local challenge state. A Provider that explicitly opts into the
contract returns a bounded private continuation with its first QR challenge.
Core encrypts that value under a permanently Provider-scoped repository and
atomically advances the exact newest owner/account/AuthSession to `WaitingUser`.
Ordinary QR credential submission is then rejected so it cannot bypass the
continuation lifecycle.

`POST /api/v1/provider-accounts/{account_id}/auth-sessions/{session_id}/poll`
claims exactly one bounded poll lease and records every attempt before Provider
I/O. A transient network failure releases the same continuation revision but
still consumes the fixed poll budget. An abandoned lease is closed as
retryable before a later sequence is issued. Definite waiting responses rotate
the ciphertext and both continuation/AuthSession revisions in one transaction;
rejection, expiry, invalid protocol and cancellation close the AuthSession and
delete the private continuation without erasing the hash-only poll ledger.

A definite remote success is first encrypted as a terminal credential
candidate and moves the session to `ValidatingCredential`. Provider
finalization is deterministic and cannot repeat the poll. If validation or the
process is interrupted, the next poll request resolves the same terminal
candidate and resumes only validation. The final transaction consumes that
candidate, replaces encrypted Provider credentials, advances both account and
AuthSession to `Authenticated`, and creates the ordinary initial Answer History
harvest. Tests prove no secret appears in API output, concurrent poll claims do
not duplicate I/O, expired leases remain auditable, and recovery does not
increment the Provider poll count. `asterismctl provider-account auth poll`
drives the same owner-scoped endpoint.

## Two-hundred-and-sixty-eighth Phase 0 slice

Receipt-conditional mutation sequences can now express a bounded retry phase
that advances on the first accepted receipt or after the configured maximum
number of definite receipts. This is distinct from the existing accepted-at-
maximum and rejected-or-maximum conditions: an accepted mutation is never
issued again merely to fill the phase cardinality, while a definite rejection
may authorize the next ordinal up to the immutable limit.

The condition participates in the frozen plan digest and has an explicit
durable encoding. Migration 071 rebuilds the constrained phase table while
preserving existing plans and phase-gated observations. Storage coverage
proves a rejected first request permits one successor, the next accepted
receipt short-circuits before the maximum, and the following phase receives
the actual contiguous ordinal. Ambiguous issued mutations remain ineligible
for retry because sequence advancement still requires a persisted receipt.

## Two-hundred-and-sixty-ninth Phase 0 slice

Execution recovery now exposes an optional, read-only mutation-sequence
snapshot to the Provider's dedicated recovery verifier. Core loads the frozen
Provider plan artifact, conditional sequence, contiguous issue/receipt/
verification records and phase observations by the exact active Attempt, then
reconstructs a credential-free Provider API value. The constructor rechecks
artifact digest binding, Provider namespaces, phase order and limits,
response-dependent transitions, observation gates and the rule that only the
last issued record may lack a definite receipt.

The snapshot grants no mutation sink and cannot resume or replay an operation.
Existing Providers retain their ordinary goal-bound verifier by default;
fixed-plan or mutation-free recoveries receive no sequence snapshot. Engine
coverage proves an abandoned sequence reaches recovery with its original
artifact and hash-only records, while the immediate post-execution verifier
does not receive recovery-only evidence. This closes the generic same-Attempt
read/dispatch boundary, but does not itself create WELearn parent/child
Executions or register its composite batch capability.

## Two-hundred-and-seventieth Phase 0 slice

A definite rejected atomic-mutation receipt can now carry a bounded retry
delay. Core stores the resulting absolute `retry_not_before` deadline beside
the hash-only receipt and refuses to issue the next contiguous mutation in the
same conditional sequence before that deadline. Accepted receipts cannot carry
a delay, and the public contract rejects zero or values above twenty-four
hours.

The deadline is derived once from the persisted receipt time, so restarts and
worker replacement do not shorten the wait. Recovery reconstructs the original
relative delay from those two durable timestamps and exposes it only through
the credential-free mutation-sequence snapshot. Duplicate receipt recording
must match the delay as well as the response digest and acceptance result.
Coverage proves migration preserves legacy receipts, early successor issue
fails closed, issue at the exact deadline succeeds, and recovery retains the
original delay. This boundary does not schedule a retry job by itself and does
not authorize replay of an issued mutation without a definite receipt.

## Two-hundred-and-seventy-first Phase 0 slice

Execution recovery can now return an optional hash-only verification for the
final accepted mutation together with the ordinary goal-bound outcome. Core
accepts that evidence only when the frozen conditional sequence is fully
advanced, its final record has a definite accepted receipt, the ordinal is
exact, and any verification already present in the same snapshot is identical.
An incomplete, rejected, fixed-plan or mutation-free recovery cannot attach a
mutation verification.

The recovery worker writes new evidence through a dedicated repository path
bound to its claimed Recovery job, live execution lease and exact active
Attempt.
Storage independently requires the target to be the latest accepted mutation;
it never impersonates the original execution worker or changes the issue and
receipt records. An exact existing verification is idempotent, while any digest
or result mismatch fails closed. The proof is committed before Core consumes
the Provider outcome and finishes or advances recovery, so a crash exposes the
same durable verification to the next read-only snapshot instead of repeating
remote mutation or readback work.

Coverage proves a fresh final proof is persisted by the recovery worker and the
Execution then succeeds. This contract carries no Provider-private stage
output: UAI upload versions/keys/answers and WELearn parent batch authority must
still use separately encrypted, atomically bound state rather than a sanitized
plan artifact or mutation digest.

## Two-hundred-and-seventy-second Phase 0 slice

The shared Core boundary now models a Provider-scoped parent authority together
with the complete bounded batch snapshot that it authorizes. Both private
values are held as zeroizing `SecretValue`s, type-names and SHA-256 digests are
frozen in the immutable Execution record, and the two values are encrypted
under separate `ProviderExecutionState` secret blobs in the same transaction.

Binding requires the current live worker claim, the exact unfinished Attempt,
the Running Execution state and the permanently scoped Provider account. A
second bind is accepted only when the Attempt, Provider, type-names and
digests are identical; another Attempt or changed material fails closed. A
resolve path requires the same worker claim and an authorized Core/Provider
runtime access, re-authenticates both encrypted blobs, rebuilds the typed
snapshot and verifies both digests before returning private bytes. Recovery may
resolve the frozen record but cannot create or replace it. Audit metadata
contains only the binding identity, type-names and hash placeholders, never
authority or batch bytes.

This is a persistence and dispatch boundary only. It does not create child
Executions, schedule a batch, or grant a Provider a mutation sink; those remain
explicit capability and Engine decisions after the parent snapshot is frozen.

## Two-hundred-and-seventy-third Phase 0 slice

Provider API now has one generic, order-preserving child dispatch plan bound to
the encrypted parent's authority and complete-batch digests. Every child keeps
its one-based ordinal, redacted remote Task identity, exact Provider execution
call plan, required Provider plan artifact and artifact-bound conditional
mutation sequence in the same immutable value. Provider namespaces are
rechecked across the artifact, sequence, operations and observation gates.

A complete batch requires contiguous ordinals, unique remote Task identities,
unique artifact and sequence digests, one Provider and at most 8,192 children.
This prevents Core from reconstructing a batch by zipping independent target,
artifact and sequence arrays where one reordered element could widen another
child's authority. The value remains planning evidence only: it does not map a
remote identity to a local Task, create an Execution, change Task state,
reserve credit, enqueue scheduler work or expose a mutation sink. Those
transactional parent/child operations remain the next shared Storage/Engine
slice.

## Two-hundred-and-seventy-fourth Phase 0 slice

Core Domain now gives a Course-scoped parent batch its own `BatchExecutionId`
and `BatchExecutionAttemptId` instead of attaching it to a synthetic Task or
borrowing one selected child as an anchor. The parent binds the exact Provider
account and Course, one canonical executable capability set, the bounded
expected child count, requester/source and the ordinary execution lifecycle.

Validation rejects read-only/non-executable capabilities, duplicate or
non-canonical capability order, zero or more than 8,192 children and lifecycle
timestamps inconsistent with the parent state. A successful parent requires a
real started attempt; cancellation or definite failure may terminate before
remote work starts. Because the parent owns no `TaskId`, it cannot consume a
child Task lease or force the first child to have different scheduling and
recovery semantics. Durable tables, scheduler claims, encrypted snapshot
rebinding and atomic child creation remain the following Storage/Engine work.

## Two-hundred-and-seventy-fifth Phase 0 slice

SQLite now persists the independent Course-scoped parent in dedicated
`batch_executions`, `batch_execution_attempts` and `batch_execution_leases`
tables. The Course/account pair is protected by a composite foreign key, while
the parent retains its canonical child capability set, bounded expected count,
requester/source, lifecycle and scoped idempotency identity. No Task row or
ordinary `execution_leases` entry is created or changed by this transaction.

Scheduling validates the Domain object, exact account/Course owner binding and
request actor, then atomically writes the parent, a dedicated
`batch_execution` scheduler job, sanitized audit and transactional outbox
event. Exact replay returns the existing parent; changed content under the same
idempotency key or a foreign Course/account binding fails closed. The scheduler
has a separate batch claim filter, so the existing Task execution worker cannot
accidentally consume a parent job before the parent worker exists.

This slice deliberately stops before starting a parent Attempt or persisting
private planning material. The next transaction must claim the dedicated job
and lease, create the exact Attempt, invoke Provider parent planning, encrypt
and bind its authority/snapshot, then map and create every ordered child from
the digest-bound dispatch plan.

## Two-hundred-and-seventy-sixth Phase 0 slice

The dedicated batch worker can now turn one live `batch_execution` scheduler
claim into the exact parent Attempt and lease without touching any Task-scoped
execution state. Starting validates the claimed job kind and payload, worker
ownership and expiry, Scheduled parent state and bounded attempt number, then
atomically creates the lease and Attempt, advances the parent to Running and
writes sanitized audit/outbox evidence. A restart by the same worker under the
same live claim returns the already-active Attempt instead of creating another
one; a lost claim, expired lease or different active Attempt fails closed.

The parent authority and complete Provider-private batch are now stored under
that Course-scoped Attempt rather than an ordinary Task Execution. Migration
075 binds their two independent encrypted `ProviderExecutionState` secret
blobs to the `(BatchExecution, BatchExecutionAttempt)` pair. Bind and resolve
both require the exact live scheduler claim, parent lease, active Attempt,
Provider/account ownership and authorized Core or matching Provider runtime
access. The first immutable binding wins; exact replay is idempotent, changed
metadata conflicts, ciphertext authentication and both frozen digests are
checked before private bytes are returned, and audit metadata contains only
type names plus hash placeholders.

Coverage proves scheduling through Attempt start, idempotent worker restart,
Task isolation, ciphertext non-disclosure and fail-closed tampering on a fresh
database. Provider parent planning is not invoked yet, and this slice does not
map remote identities, create child Tasks/Executions, reserve child credit or
enqueue child jobs. The next shared transaction must call the Provider parent
planning hook, verify its digest-bound ordered child plan and atomically create
or recover every child before any mutation can be issued.

## Two-hundred-and-seventy-seventh Phase 0 slice

Provider API now exposes the Course-scoped parent planning boundary without
granting Providers access to Core repositories or scheduling. One request
binds the exact `BatchExecution` and Attempt, local and redacted remote Course,
canonical child capability set, frozen expected count, resolved account-level
runtime settings and a bounded Provider-namespaced private planning input. The
input is zeroizing, digest-bound and Debug-redacted; Core must encrypt it before
the worker resolves it, and its digest alone grants no planning or mutation
authority.

The fresh asynchronous hook returns one `PreparedProviderBatchExecutionPlan`
that keeps the private parent snapshot and complete ordered child projection
together. Construction independently rechecks Provider identity, both parent
digests and the already scheduled child count. A second deterministic recovery
hook accepts only the resolved encrypted parent pair and must reconstruct the
same child projection without I/O, rescanning, order repair or target
redistribution. Providers which do not implement this exact Course batch
contract fail with `UnsupportedTask` by default, preserving existing
single-Task behavior.

This slice defines and verifies the runtime contract only. The private planning
input does not yet have a scheduling-time encrypted repository, Engine does not
invoke the hook, and Storage does not consume the returned children. Those
pieces must land before WELearn's Provider implementation can be registered and
before any child Task/Execution or mutation sequence is created.

## Two-hundred-and-seventy-eighth Phase 0 slice

Batch scheduling now requires the Provider-namespaced private planning input
and freezes it in the same SQLite transaction as the parent, scheduler job,
audit and outbox event. Migration 076 stores only its type, domain-separated
digest, encrypted `ProviderExecutionState` blob reference and bind time. The
account's actual Provider must match the input namespace, and idempotent replay
requires the same parent plus the same input type and digest; changed private
selection under an existing request key is an explicit conflict.

The batch repository now owns the encryption keyring because no scheduled
parent may exist without its corresponding private product authorization. The
worker resolve path requires the exact live batch scheduler claim, lease and
active Attempt, Running parent state, account owner and an authorized Core or
matching Provider runtime actor. It authenticates the secret blob, rebuilds the
bounded typed input and rechecks its digest before returning bytes; secret
audits carry only purpose/key metadata and the access reason. Tests prove the
plaintext is absent from persisted ciphertext, changed idempotent input is
rejected and ciphertext tampering fails authentication.

This closes scheduling-time input durability but does not invoke the Provider
hook or persist its public ordered child projection. Engine still has to
resolve the input and account settings, perform the one fresh parent planning
call, bind the returned private parent pair, then atomically materialize every
child or recover the exact already-created set.

## Two-hundred-and-seventy-ninth Phase 0 slice

Engine now owns the claim-bound parent planning orchestration. It starts or
reuses the exact `BatchExecutionAttempt`, rechecks the persisted parent,
Provider account and Course binding, resolves only Provider/account runtime
settings, and opens the encrypted planning input and parent snapshot through a
Core-service access scoped to the worker correlation. Providers receive the
opaque credential references and fresh Course identity, but no Storage,
scheduler, lease or secret-write capability.

On the first pass Engine invokes the Provider's read-only batch hook once. It
then asks the same Provider to reconstruct the ordered child plan solely from
the returned private parent pair and rejects any difference before persistence.
Only after that deterministic cross-check does Core encrypt and bind the parent
snapshot. A restart first resolves the already-bound pair and uses the
parent-only recovery hook, so it cannot rescan membership or redistribute
targets. Integration coverage drives real SQLite scheduling, encrypted input,
claim/lease/Attempt creation and parent binding, then proves a second planner
run returns the identical two-child plan while the fresh Provider call count
remains one.

Storage also exposes a runtime Course lookup and a Provider-scoped parent
snapshot factory selected only after account binding. No ordered child record
is persisted by this slice: a crash after parent binding is recoverable because
the Provider can reconstruct the plan, but child Tasks, Executions, artifacts,
mutation sequences, credits and jobs still require one atomic materialization
transaction before remote mutation is allowed.

## Two-hundred-and-eightieth Phase 0 slice

The parent planner now durably materializes the complete ordered child plan
immediately after the encrypted parent pair is bound. Migration 077 stores one
row per parent ordinal with the exact already-discovered local Task binding,
hash-only remote identity, Provider call groups, credential-free artifact,
artifact digest, sequence type/digest and materialization time. Every
receipt-conditional phase is stored separately with its occurrence bounds,
rejection behavior, advance condition and optional observation gate.

Materialization requires the same live parent job claim, lease and Attempt as
planning. Storage independently rechecks the Running parent, expected child
count, account/Course Provider, encrypted-parent authority and batch digests,
uniform canonical capability set, exact local Task remote identity and Task
capability coverage. It resolves all children before writing any row, then
inserts the full order and sanitized audit in one transaction. Existing rows
are accepted only when every Task, call group, artifact payload/digest,
sequence/digest and phase matches the recovered Provider plan; partial rows or
any changed field fail closed.

Engine performs this transaction on both the fresh and restart paths. Coverage
proves the restart returns identical durable child records without another
fresh Provider call and detects a tampered child artifact digest. These are
planning records only: no child `Execution`, Task state transition, credit
reservation or scheduler job exists yet. The next transaction can now create
those Executions from one durable, parent-bound source instead of invoking or
trusting the Provider again.

## Two-hundred-and-eighty-first Phase 0 slice

Parent planning now freezes one account-level runtime settings snapshot before
the first Provider inventory call. Migration 078 binds the resolved values,
per-key sources, Provider and account override revisions, Core completion
policy and capture time to the exact `BatchExecutionAttempt`. Task-scoped
sources and revisions are forbidden at this Course parent boundary.

Storage requires the live parent claim/lease/Attempt and Running state, checks
the actual account Provider, validates the resolved snapshot against the
registered schema, and accepts a second bind only when the whole typed value is
identical. Engine first loads this frozen value on every run; only when absent
does it read current Provider/account layers, resolve sources and derive the
completion policy, then bind the candidate before Provider planning. A restart
therefore does not inherit later defaults or overrides even when it only needs
the parent-only restore hook.

The planning result carries the frozen settings beside the ordered public plan
and durable child records so the following child-Execution transaction has one
stable source. Child Task-specific overrides are intentionally not merged here:
the next transaction must either prove they were already represented in the
parent authorization or reject conflicting per-Task settings rather than
silently broadening one child after the batch was planned.

## Two-hundred-and-eighty-second Phase 0 slice

Migration 079 and the parent planner now create one immutable child
`Execution` for every durable batch-plan ordinal. Storage performs this under
the same live parent job claim, lease and Attempt, requires the Running parent,
complete ordered child-plan set and frozen parent settings, then preflights all
children before writing any of them. Each Task must still belong to the bound
Provider, remain remotely executable and be locally ready or failed, must have
no conflicting Task-scoped runtime override and must not already own another
active Execution.

The atomic write stores each child in Requested state with a parent-scoped
idempotency key, exact canonical capability set, ordered call groups, the
credential-free Provider artifact and the frozen account-level settings. A
separate `(batch_execution_id, child_position)` mapping preserves parent order,
and Requested outbox events plus a sanitized audit record are committed with
the children. This draft boundary deliberately does not change Task state,
reserve credit or enqueue child execution jobs, so no Provider mutation can
become scheduler-visible before the later activation transaction succeeds.

Restart accepts existing children only when every immutable binding still
matches the parent plan: Task, caller identity, request source, idempotency
scope/key, capability calls, artifact payload and digest, frozen settings and
capture time. Missing auxiliary rows, partial child mappings or altered values
fail closed. Engine returns the child Execution mappings on both fresh and
parent-only recovery paths. Integration coverage proves two children are
created once, remain Requested while their Tasks stay ready, create no
execution jobs, survive restart without another Provider scan and reject a
tampered persisted Provider artifact. The next slice can atomically activate
these drafts by reserving credit, moving Tasks to scheduled and publishing the
ordered child jobs while preserving the parent sequence boundary.

## Two-hundred-and-eighty-third Phase 0 slice

Migration 080 adds one durable activation row per parent child position,
binding the immutable child Execution to its exact scheduler job and activation
time. Storage activates only a complete materialized batch under the same live
parent claim, lease and Attempt. Before writing, it requires every child to
remain Requested, unscheduled and unbilled; every Task to remain locally ready
or failed and remotely executable; no competing active Execution; and no
pre-existing execution job.

Activation accepts either no billing for the whole batch or one exact quote and
reservation for every ordered child. Partial billing is rejected. A billed
activation validates owner, Task, Execution, amount, quote and timestamp
bindings, then reserves every balance together. In the same transaction it
moves all Tasks and Executions to Scheduled, attaches immutable quote IDs,
creates one execution job per frozen position, records the activation mapping,
emits credit and state outbox events and writes one sanitized parent audit.
Insufficient credit for any later child rolls back earlier reservations, state
changes, jobs and activation rows.

Idempotent recovery verifies the exact activation mapping, scheduled timestamp,
job payload and idempotency key plus every immutable quote/reservation field;
later job, Execution and reservation lifecycle states may advance without
invalidating the original activation. The parent planner likewise accepts its
child drafts after this authorized quote/state transition while continuing to
verify calls, Provider artifact and frozen settings. Engine exposes activation
as a separate second phase because child Execution IDs must exist before a
caller can construct exact credit reservations. Integration coverage proves
atomic rollback on insufficient aggregate balance, successful two-child
reservation and scheduling, replay without double debit, and parent-only
recovery without another fresh Provider scan.
