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
| available credit | reserved credit and immutable transactions |

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
