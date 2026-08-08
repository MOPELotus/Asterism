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
