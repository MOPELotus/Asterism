# chaoxing drift register

| Risk | Evidence | Required response |
|---|---|---|
| Browser-bound `uf` Cookie blocks reqwest | 2026 agent donor explicitly uses same-origin Browser fetch | Compare Native HTTP and Browser fetch with the same sanitized live case; select transport per capability |
| `enc` expires or changes | Current Work routes require a fresh course/session value | Never persist `enc` as identity; reacquire it before Work scanning |
| Mobile API behavior is stale | CxKitty implementation revision is from 2024 | Treat all `mooc1-api` mobile paths as unverified fallbacks |
| Domain variants change | Donors use `mooc1`, `mooc1-api`, `mooc2-ans` and historical institutional mirrors | Allow only audited hosts and classify unexpected redirects as protocol drift |
| Status keyword false positives | Exam pages embed localized status words in scripts | Remove script/style content and parse item structure before fallback text classification |
| `未交` is ambiguous | Current donor observed expired Work still labelled `未交` | Follow the detail redirect before declaring a task actionable |
| Per-attempt QIDs change | Current Exam donor observed complete QID regeneration on retake | Reparse every attempt; never make QID the task identity |
| Browser DOM handlers change | OCS and agent donors use different current page modes | Fixture each supported page mode and fail closed on unknown form structure |
| Formal assessment ambiguity | Exam is a source module, not an assessment nature | Inventory is allowed; automatic submission remains independently policy-gated |
| Card version or execution token changes | Primary donor currently sends card version `2025-0424-1038-3`; immediate resources use `jtoken`, while Video uses `objectId` plus a status `dtoken` and MD5 report signature | Re-fetch before every execution, never persist token/version-derived routes, and require fresh-card completion verification |

## Live-validation gate

Before `VerificationLevel` can advance:

1. validate password login, stored Cookie reuse, expiry detection and recovery;
2. scan a real course containing independent Work and Exam entries;
3. compare Native HTTP facts with the visible platform and Browser same-origin
   results;
4. record open/due/close behavior for at least one pending and one closed task;
5. verify one allowed non-formal submission through a remote result readback;
6. force or observe auth expiry, rate limiting and one protocol/error branch;
7. save only sanitized fixtures and a dated verification record.
