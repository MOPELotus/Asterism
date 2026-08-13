# Asterism agent instructions

These rules apply to every agent and sub-agent working in this repository.

## Project objective

Asterism aggregates and automates mechanical, repetitive, low-value learning
platform operations so users do not have to repeat work that audited upstreams
already automate. Whether a user enables a capability is a product authorization
decision. Whether a Provider implements an evidenced donor capability is a
protocol and engineering decision. Do not mix them.

## Upstream-first capability scope

For every donor, PortSource or Reference recorded in `UPSTREAMS.md` or
`research/providers/*`, a capability with a clear implementation, protocol
behavior or sufficiently reliable behavioral evidence is in scope by default:

```text
donor capability
-> audit
-> semantic mapping
-> Rust / clean-room implementation
-> Core capability
-> fixture / Native HTTP / BrowserBridge / Capture boundary
-> verification
-> live validation
```

Do not remove, weaken or defer a capability merely because it changes
completion, progress or score; completes work; answers, skips or submits;
performs a non-idempotent mutation; needs browser state, Capture,
BrowserBridge, OAuth, a local helper or dynamic crypto material; is complex; or
is beyond the current milestone. Native Rust HTTP is preferred, but an
incomplete native path must continue into the smallest necessary
BrowserBridge/Capture fallback.

Pinned upstream revisions are reproducible audit snapshots, not permanent
freeze points. At each meaningful Provider checkpoint, re-check the recorded
donors' default branches, tags/releases and relevant new commits. Record the
new revision and protocol/capability delta, then continue incremental audit,
porting, fixtures and regression coverage when upstream behavior changed.

## Architecture invariants

Capability completeness does not weaken reliability. Preserve Thick Core,
Rust Provider business code, bounded I/O, fresh rediscovery and identity
rebinding, immutable Drafts, mutation receipts separated from verification, no
ambiguous replay of non-idempotent operations, durable attempts and recovery,
secret zeroization, typed errors, protocol-drift fail-closed behavior,
owner/account/course/task/attempt binding and separate Capability semantics.

Risk is handled through execution, persistence, recovery and verification—not
by deleting the capability. If existing shared contracts cannot express the
real protocol, report the Core Gap to Main; Main extends Core and the Provider
then continues.

## Provider completion and workers

Read-only, non-Capture, Native HTTP, inventory, DurationReport, MVP and other
intermediate milestones are checkpoints only. A Provider worker stops only
when Main explicitly stops it, or when all audited donor abilities have code
and all currently executable verification, or when every remaining ability has
a specific hard blocker that continued donor audit, Core work, Capture,
BrowserBridge or protocol implementation cannot resolve. A milestone may be
reported, then work continues.

Provider workers primarily edit their own Provider code, tests, fixtures and
research. Shared Core, Domain, Provider API, Engine, Storage, API, OpenAPI,
Capture and cross-Provider abstractions remain owned by Main unless Main
explicitly delegates them.
