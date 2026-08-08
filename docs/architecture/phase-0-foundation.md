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

The next slice adds layered configuration and authenticated CLI management
flows without bypassing the HTTP API.
