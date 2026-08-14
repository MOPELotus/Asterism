# chaoxing drift register

| Risk | Evidence | Required response |
|---|---|---|
| Browser-bound `uf` Cookie blocks reqwest | 2026 agent donor explicitly uses same-origin Browser fetch | Compare Native HTTP and Browser fetch with the same sanitized live case; select transport per capability |
| `enc` expires or changes | Current Work routes require a fresh course/session value | Never persist `enc` as identity; reacquire it before Work scanning |
| Mobile API behavior is stale | CxKitty implementation revision is from 2024 | Treat all `mooc1-api` mobile paths as unverified fallbacks |
| Domain variants change | Donors use `mooc1`, `mooc1-api`, `mooc2-ans` and historical institutional mirrors | Allow only audited hosts and classify unexpected redirects as protocol drift |
| Status keyword false positives | Exam pages embed localized status words in scripts | Remove script/style content and parse item structure before fallback text classification |
| Exam score/retake false positives | The PortSource scans localized score text and structural `reTest(...)` handlers beside remaining-time text | Bound scores to 0-100, reject conflicts/malformed precision, exclude `分钟`, require an exact row-local handler, and keep both facts separate from state and authorization |
| Exam score is mistaken for answer proof | Result score and visible per-Question answers are independent donor facts | Preserve score as exact thousandths on any verification status, but derive Question confirmation only from the complete bound ID/order/type/value result set |
| Exam result route or facts drift | Current PortSource distinguishes Exam preview/detail pages, but entry variants and result DOM may differ | Read only the allowlisted preview path with exact course/class/exam binding, reject redirects and conflicting list/detail scores, and retain unsupported variants as list-level evidence until separately fixtured |
| `未交` is ambiguous | Current donor observed expired Work still labelled `未交` | Follow the detail redirect before declaring a task actionable |
| Per-attempt QIDs change | Current Exam donor observed complete QID regeneration on retake | Reparse every attempt; never make QID the task identity |
| Browser DOM handlers change | OCS and agent donors use different current page modes | Fixture each supported page mode and fail closed on unknown form structure |
| Formal assessment ambiguity | Exam is a source module, not an assessment nature | Keep source classification separate from the explicit assessment facts used by product authorization; do not remove evidenced Provider capabilities |
| Card version or execution token changes | Primary donor currently sends card version `2025-0424-1038-3`; immediate resources use `jtoken`, while Video uses `objectId` plus a status `dtoken` and MD5 report signature | Re-fetch before every execution, never persist token/version-derived routes, and require fresh-card completion verification |
| Work final-submit form changes | The primary donor copies current editor fields, while older mobile donors name a fixed but different parameter set | Parse a fresh editor, forward only the audited bounded allowlist, bind course/class and the exact Question/type set, and fail closed if required attempt material changes |
| Work acknowledgement is mistaken for completion | Donors log `status=true` as success, while current task pages may remain on prompt/waiting states | Persist only a Receipt, then independently re-discover the Work; prompt/editor remain Pending and only exact server-visible result answers confirm completion |
| Chapter Work completed card is mistaken for answer proof | Primary donor exposes only `isPassed`, while the 2026 Chapter-test donor reaches visible answers and final score through a separate `selectWorkQuestionYiPiYue` iframe | Keep card completion task-level; require a freshly bound iframe document, complete QID/order/type/value facts, and the strict Provider parser before emitting per-Question verification |
| Chapter Work standard answers are reused across retakes | The Chapter-test donor reports that option `data` values are randomized after `redoTest` while displayed letters remain stable | Parse standards only from the fresh bound result, require every value to match the current Question option IDs, and never cache or execute a later retake from an earlier mapping |
| External or guessed answers are mislabeled Provider-native | Samueli uses configured Tiku plus random fallback; CxKitty uses REST/JSON/SQLite/OpenAI plus fuzzer; OCS uses configurable wrappers/cache; the agent skill consumes pre-computed answers | Keep those sources in Core external/manual evidence contracts; Chaoxing AnswerResolve accepts only a fresh completed platform result's `正确答案`, and remains unregistered until its post-result lifecycle is explicit |
| Post-result standards are used as pre-submit authority | The only native standard-answer evidence appears after a completed Chapter Work result, while normal AnswerResolve precedes Draft construction | Keep the typed component unregistered; require fresh completed TaskDetail and bound result transport, and add an explicit Main lifecycle before exposing the capability |
| Result-page answer DOM changes | OCS and browser donors distinguish `.mark_answer`/`我的答案` from editable hidden inputs and CSS selection state; Exam retakes rotate QIDs | Require an allowlisted Work view or strictly bound Exam preview, complete unique QID set, DOM/Draft order, exact type and bounded visible-answer grammar; missing/extra/unsupported facts stay Inconclusive and duplicates fail closed instead of inferring from CSS, score or list text |
| Exam start response is ambiguous | `phone/start` creates an attempt before dynamic `enc` Question acquisition completes | Persist the exact start request digest before send, never auto-replay an issued operation, and add only fresh readback-based recovery when live evidence proves it |
| Exam preview rotates attempt state | CxKitty overwrites `enc`, remaining time and last-update fields from the full preview before saving answers | Parse and bind preview state after start; reject any save response which regresses time or advances more than one Draft cursor |
| Exam answer/final POST is ambiguous | Temporary saves and final submit share a non-idempotent route with randomized signatures | Freeze signature/query/form and digest before send; never replay a temporary save; recover only a final submit when fresh exact Exam inventory is Completed |

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
