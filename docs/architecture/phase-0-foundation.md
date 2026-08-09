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
session. Polling and cancellation return only the persisted non-secret
`AuthSession`; every response is marked `no-store`, and OpenAPI describes the
conflict, Provider failure, expiry, and rate-limit outcomes.

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
positive `capture_recipe_version`. Core treats this metadata as the authoritative
server-side requirement when creating a local-helper pairing; clients cannot
choose or downgrade it in a request.

A Capture recipe declaration is valid only when the same Provider advertises
and implements Authentication. Providers without a bundled recipe remain
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
has no Capture recipe, and is not registered until CourseInventory and a
live-tested transport exist.

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

This enables first-batch native authentication to stop accurately on captcha,
SMS, browser or confirmation gates without implementing Capture or automated
challenge solving. The reason is diagnostic state only and does not broaden
Provider authority or expose response bodies.

## Sixty-seventh Phase 0 slice

Chaoxing now implements development-level Password and ImportedCookie
authentication without Capture. Password fields use the donor-compatible
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
live-support or verification claim and requires no Capture path.

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
capability, Capture dependency, live-account verification or higher Provider
verification level is claimed.

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
behavior remain unverified, so Chaoxing stays `Development`; no Capture path is
introduced.

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
as typed `HumanRequired(ImageCaptcha)`; the deferred-Capture phase adds neither
OCR nor donor automatic solving. This is offline fixture and transport-boundary
evidence only, so Chaoxing remains disabled by default and `Development`.

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

The active first-batch delivery order now explicitly excludes Provider work
which requires a local Capture runtime, browser traffic capture or system proxy.
Native HTTP, manual credential/session import and offline fixture work continue;
Capture-dependent authentication, diagnostics and live validation remain open
acceptance items and cannot contribute to a `Verified` claim.

The already implemented generic Capture pairing and credential boundaries stay
in the repository, but this phase does not expand them with Provider-specific
recipes or interception logic. In particular, cidaren may progress through
manual token import and non-Capture capabilities while its WeChat-assisted
Capture path remains deferred and the Provider remains incomplete.

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

All supported page modes remain parse-only and answer-free at this checkpoint.
The correction adds no remote execution, Capture dependency, hidden-answer
reading or submission behavior.

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
