# WELearn one-time full upstream sweep

Audit date: 2026-08-15. This is the mandatory full re-audit required by
`research/providers/NEXT_CHECKPOINT.md`. It records in one checkpoint the
complete audit ledger previously maintained in `SOURCES.md`; the pass re-read
the whole donor surface rather than only commits after the pinned revisions.
No real account, credential, challenge response or learning mutation was used.

## Evidence enumerated

| Evidence line | Material re-read |
|---|---|
| `Fanyuchang2026/welearn-helper` | Complete `master` tree, README, configuration/dependency files, login and Course/Unit/SCO/CMI implementations, `v4.0.0`, its sole Release and all public issue entries |
| `YZBRH/Welearn_helper` | Complete `main` tree, README, dependency/packaging files, login/inventory/CMI/duration implementation, absence of tags/Releases and all public issue entries |
| `1q2w-c/Auto_WeLearn` | Complete `master` tree, README, modular API/UI/account/batch workers, configuration surface, absence of tags/Releases and public issues |

The 2026-08-15 refresh still resolves to:

- `Fanyuchang2026/master@afa87fb7c86d6b3fdaa63ed08b4a02b4b112e2a2`;
- `YZBRH/main@bd160e91d0452b8bf483087fbdd3bdd58d855e13`;
- `1q2w-c/master@85918caaccd93b73b1e41fe537b4e9a11377b759`.

Fanyuchang's only tag and Release remain the older
`v4.0.0@5d1df60cb0078f70679be2a22f2afbf1ef4fa88a`. YZBRH and
Auto_WeLearn still expose no tags or Releases. The GitHub issue endpoints
currently return one, nine and zero entries respectively, including pull
requests. No default-branch, tag, release or protocol delta is available to
port.

No donor contains a protocol fixture suite. Asterism uses clean-room synthetic
fixtures derived from bounded source facts; donor binaries were inventoried but
not executed or reverse engineered.

## Complete capability surface

| Donor behavior | Current Asterism mapping | Classification |
|---|---|---|
| Password/OIDC, raw Cookie input, captcha/SMS and external-provider browser choices | Authentication, stored-session renewal and versioned Capture recipes | Native Password/ImportedCookie and bounded assisted/external-browser Cookie paths are implemented offline; interactive callback and live validation remain shared gates |
| Course -> Unit -> SCO discovery, ordered Unit selection and all-task selection | CourseInventory, TaskInventory and Provider-private batch plan | Native inventory and immutable selection/membership plans are implemented; durable parent/child scheduling remains a Core gap |
| Fresh SCO identity/detail and CMI read | TaskDetail, TaskProgressRead and DurationRead | Implemented with full fresh rediscovery, fingerprint/capability/state rebinding and fail-closed result parsing |
| Direct completion/progress/score writes | ResourceExecution | Current dual-save, legacy set/save and Auto save-only profiles are separately frozen and independently verified; ambiguous mutation is never replayed |
| YZBRH preservation duration and Auto single-file duration | DurationReport | Implemented with donor-specific start/keep/finalize/readback evidence and goal-specific Core verification |
| Current Fanyuchang and modular Auto duration plus completion in one start session | atomic duration-completion child | Provider parent/batch/child artifacts, complete ordered dispatch projection, conditional sequence, issue/receipt/observation transport, native runtime, same-attempt snapshot adapter and immediate/read-only verification are implemented. Foreign generic retry deadlines fail closed. The runtime remains deliberately unregistered until Core creates and dispatches durable parent/child attempts and persists recovery proof |
| Auto aggregate duration budget and equal-floor child allocation | bounded parent batch authority and child targets | Provider modeling is implemented; shared parent runtime scope, entropy, orchestration and API remain missing |
| Per-account 1-100 worker setting and independent sessions | Provider/account concurrency bound | Implemented; global and cross-account admission remain Core-owned |
| Account add/remove, nickname/status, CSV/TXT import/export and batch-account queue | shared account/API/UI behavior | Not WELearn wire behavior; plaintext donor password export must not be copied |
| Captcha/SMS/OAuth navigation and authenticated response readiness | HumanRequired plus Capture/BrowserBridge boundary | Exact public origins and response readiness are frozen; no donor supplies an automated challenge solver |
| Question inventory, per-Question answer submission or result harvesting | none | Unsupported due to missing protocol evidence; direct CMI mutation is not Question/Answer behavior |

## Complete settings surface

| Donor setting/choice | Current mapping | Classification |
|---|---|---|
| Fixed score, uniform range and clamped-Gaussian score | immutable Resource execution score policy | Implemented with deterministic Execution-bound sampling |
| Fixed/random per-SCO duration and 1/60-second heartbeat families | immutable Duration plan and cadence | Implemented only in evidenced donor combinations |
| Fanyuchang selected score followed by unconditional score 100 | current dual-save profile | Implemented; older fractional/progress `v4.0.0` variants are superseded drift, not current selectable profiles |
| Plain JSON versus `[INTERACTIONINFO]` CMI, fresh/zero/preserved time and set/save versus save-only | complete mutation profile | Implemented and cross-donor mixtures fail before I/O |
| Auto base 1-300 minutes, signed random range 0-30 and one frozen aggregate sample | `WellearnAutoDurationBudget` parent authority | Provider-private representation implemented; shared durable parent policy remains a Core gap |
| One, ordered subset or all Units; hidden/completed filtering differences | flow-specific selection and raw/merged visibility facts | Implemented without rewriting a hidden Task as open |
| Timeout/retry, TLS-client packaging, stop/log UI and forced worker termination | shared network/runtime/application behavior | Existing bounded network policy applies; donor exception swallowing or forced termination is not ported |

## Evidence, completion and retake boundary

No donor exposes historical Attempts, Question snapshots, submitted/official
answers, per-Question grading, a passing threshold, remaining retakes or a
distinct retake-creation route. The only result facts are SCO-level CMI
completion, progress, score and duration.

Consequently:

- WELearn cannot feed private Answer Evidence or the Global Corpus from the
  audited donor surface;
- first binding may inventory completed SCOs but cannot claim an answer-history
  bootstrap harvest;
- fresh exact SCO completion supports Strict Completion verification;
- a CMI score cannot establish answer correctness, pass policy or retake
  availability;
- Score Improvement must remain unavailable for WELearn until an exact result
  and retake protocol is established by a current donor or sanitized live
  evidence.

## Shared gaps reported to Main

1. Durable parent/child batch execution must atomically bind the encoded parent
   authority and up-to-8-MiB complete batch snapshot to one parent attempt, then
   create every ordered child Execution with its exact Provider artifact and
   conditional sequence. Child retries/recovery must not rescan or redistribute
   membership. Provider v2 now binds every ordered Fanyuchang target through a
   four-KiB count/digest authority plus the full vector in the complete batch;
   Auto remains aggregate-derived.
2. Core now owns the conditional ledger, attempt-bound observations,
   same-attempt recovery snapshot loading and recovered final-verification
   persistence. It still needs WELearn composite registration/dispatch, parent
   planning input, transactional child creation and resolved private-snapshot
   injection into execution/recovery.
3. Capture/BrowserBridge still needs active browser session injection, action
   dispatch, callback handling and bounded recovery for interactive login.
4. Cross-account bulk import/export and queueing are shared product surfaces and
   require secure credential policy outside the Provider.
5. No answer-history, Question evidence, pass-threshold or retake contract can
   be added without new protocol evidence.

## Re-audit outcome

Every executable donor platform call and semantic runtime setting is now
implemented, represented by an existing Provider-private plan, assigned to a
specific shared Core gap, or blocked by missing protocol/live evidence. The
full sweep found no currently unported WELearn wire capability and the latest
refresh found no upstream delta.

This one-time full-sweep requirement is complete for WELearn. Together with the
Chaoxing, UAI and Cidaren sweep records, all four first-batch Provider sweeps
required by `NEXT_CHECKPOINT.md` are now explicitly recorded. Future meaningful
checkpoints return to normal revision/delta refresh while implementation and
live validation continue.
