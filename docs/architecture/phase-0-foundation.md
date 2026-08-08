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
