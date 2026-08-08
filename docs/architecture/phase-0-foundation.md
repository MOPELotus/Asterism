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
- `GET /api/v1/providers`
- `GET /api/v1/openapi.json`

Every response receives an `x-request-id`. The initial CLI supports
`system health` and `provider list` against the same API.

## Next Phase 0 slice

The next slice adds execution and orchestration transition guards, atomic lease
and credit repositories, scheduler job models, the secrets boundary, initial
authentication/permission services, and durable outbox dispatch.
