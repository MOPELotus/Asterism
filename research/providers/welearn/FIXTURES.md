# WELearn fixture plan

No real-account fixture was added during the 2026-08-11 static donor audit.
Initial implementation uses explicitly synthetic JSON fixtures. They prove
parser invariants only and are not live-platform evidence.

## Initial synthetic fixtures

```text
fixtures/providers/welearn/
  auth/password-success.json
  auth/password-rejected.json
  auth/password-captcha.json
  auth/password-sms.json
  auth/anonymous-course-list-login.html
  courses/course-context.html
  courses/list-mixed.json
  courses/list-mixed.expected.json
  units/list-mixed.json
  tasks/leaves-unit-0.json
  tasks/leaves-unit-1.json
  tasks/list-mixed.expected.json
  cmi/progress-mixed.json
  cmi/duration-before.json
  cmi/duration-after.json
  cmi/resource-completed.json
  cmi/resource-completion-cmi.expected.json
```

The Auth fixtures cover bounded response classification, exact
`/idsvr/transfer.html` and `/idsvr/connect/authorize/callback` route binding,
and `HumanRequired` separation without retaining callback/token material.
Wrong paths on the trusted SSO host and callback-prefix lookalikes fail closed.
The SMS fixture uses the audited `code=0 + extraCheck.vcToken`
continuation shape and must never be classified as redirect-ready success.
`anonymous-course-list-login.html` is the sanitized 2026-08-13 public response
returned by the Course-list endpoint to an anonymous prelogin session; native
classification treats the exact script-redirect shape as Authentication so a
premature Capture snapshot is rejected and Composite renewal remains possible.
The Course fixture covers numeric/string IDs, completion percentage
and unknown extra fields. Unit/SCO fixtures cover visible/hidden Units,
completed/unknown/not-open states, duration separation and stable
Course/Unit/SCO identity. Inline negative tests cover malformed rows, duplicate
identities, invalid percentages, misbound Unit responses and duration without a
completion observation. The CMI fixture covers the outer `comment` envelope,
independent completion/progress/session/total facts, unknown-field dropping and
separate generic progress and DurationRead conversion. Inline negative cases
reject missing envelopes, non-zero result codes, malformed nested JSON,
unsupported scalars and out-of-range progress.
Three precise observation regressions cover Course `clist`, Unit/SCO `info` and
CMI outer/`ret` drift. They assert exact surface/kind/shape values and use
sentinel strings plus nested `answer` content to prove no field value, answer or
raw document crosses the Provider error boundary.

The duration pair is a synthetic before/after readback. It keeps completion,
progress, score and success status identical while changing only opaque raw time
observations. Injected lifecycle tests freeze provider/task runtime overrides,
reject completion/score drift, reject absent/partial preservation fields and
reject a final read with no time change.
Native-boundary tests separately reject malformed mutation results, classify
well-formed unsupported integer codes as explicit negative receipts, and prove
the preserved CMI form state. Missing/non-integer `ret` tests assert the exact
value-free TaskExecution observation and sentinel non-disclosure. Wire-plan
regressions keep
YZBRH's conditional start/initial keep, current Fanyuchang's unconditional
start and `0,1,2...` paired client counters, and Auto_WeLearn's delayed
60-second implicit plan distinct. Native request tests also keep the audited
plain-endpoint four-field minimal start separate from the query-uid six-field
full-route start.
The mutation-baseline regression recognizes YZBRH's exact
`学习数据不正确` marker as uninitialized only inside mutation-capable
DurationReport/ResourceExecution paths, rejects unrelated non-JSON text, and
requires the next post-start document to pass the ordinary strict CMI parser.
Response-reader tests allow only that marker through an unexpected media type
while still classifying structural login HTML as Authentication.

The ResourceExecution fixtures prove the independently written bounded CMI
preset and the nested fresh-read goal used by immediate verification and crash
recovery. Tests require the exact selected completion/progress/score tuple,
prove fixed and bounded-random settings stay stable for the frozen Task, and
prove recovery calls only the non-mutating CMI reader.
Native plan and injected-boundary tests cover both the YZBRH/Auto selected-score
save and current Fanyuchang selected-score-then-100 dual-save sequence. The
second sequence exposes selected and final score separately, requires a final
100 readback and still emits two saves when the selected value is already 100.
The completion-CMI fixtures also cover the ordinary zero-time document and the
duration-then-completion fresh-time document. Injected transport tests require
fresh `session_time` and `total_time` to remain identical after completion and
reject missing or changed values instead of falling back to zero.
The same bounded CMI fixture is serialized both as current Fanyuchang plain
JSON and as YZBRH/Auto_WeLearn JSON followed by the exact
`[INTERACTIONINFO]` marker; the suffix variant strips only that audited literal
in the test before comparing the unchanged JSON structure.
Injected execution fixtures separately prove `set_then_save` and `save_only`.
They also return explicit negative receipts while presenting an exact fresh CMI
goal: execution succeeds only because the readback matches, and exposes false
start/set/save acceptance fields for diagnostics. Malformed or ambiguous
responses remain covered by HumanRequired no-replay tests. Dedicated resource
and atomic mapper tests prove the validated shape survives reclassification,
the reason remains `ManualIntervention`, the error remains non-retryable and no
sentinel response value enters either message or observation.

## Full-sweep regression disposition

The 2026-08-14 mandatory full sweep found no additional remote document shape,
Question/result response or mutation route, so it does not justify inventing a
new protocol fixture. Existing authentication fixtures already prove that raw
or anonymous Cookies are not accepted without an authenticated Course read,
which is stronger than the donor Cookie path discussed in Fany issue #1.

Inline batch tests now freeze Auto's previously lossy aggregate setting
provenance: configured minutes `1..=300`, random range `0..=30`, a sampled
signed offset inside that range, `max(1, configured + offset)`, and the maximum
330-minute aggregate. Negative cases reject invalid base/range/offset values
before parent planning. No answer-evidence fixture exists because the complete
donor sweep exposed no historical result, per-Question answer, grading,
threshold or retake document to sanitize.

Parent-authority serialization tests cover both atomic donor target families,
exact v1 field names, the complete Auto budget tuple, maximum 512-index Unit
selection and bounded round trips. Negative cases reject empty/oversized bytes,
unknown fields, version changes, duplicate Unit indices, cross-flow target
mixtures and an `actual_minutes` value that disagrees with the frozen
base/range/offset. These are inline value fixtures because the artifact is a
Provider-owned persistence schema, not a remote WELearn response.

Completion-diagnosis regression keeps verified Pending and InProgress
DurationReport outcomes on the conservative `None` Provider override. Even when
the sanitized result proves `completion_preserved`, `progress_preserved`,
`score_preserved` and `duration_observation_changed`, it cannot select
`DurationInsufficient` without a required/remaining-duration fact. No remote
diagnosis fixture is added because none exists in the audited donor surface.

## Required live-sanitized fixtures

```text
fixtures/providers/welearn/
  auth/password-success-live.json
  auth/password-rejected-live.json
  auth/password-captcha-live.json
  auth/password-sms-live.json
  courses/list-empty.json
  courses/list-mixed.json
  courses/course-context-live.html
  units/list-visible-hidden.json
  tasks/leaves-pending-completed.json
  cmi/not-attempted.json
  cmi/uninitialized-live.txt
  cmi/in-progress.json
  cmi/completed-with-duration.json
  cmi/resource-completed-selected-score.json
  cmi/resource-mutation-accepted.json
```

Before committing live-derived fixtures, remove Cookies, passwords, encrypted
passwords, OIDC/PKCE state, `uid`, `classid`, real IDs, names, institutions,
scores, free-form answers, device identifiers and request identifiers. Preserve
only structural field names, response codes and bounded placeholder shapes.

## Regression requirements

- String and numeric `cid`/SCO IDs normalize identically.
- SCO assessment nature remains `Unknown` unless a future audited field proves
  it; a Resource source type never implies Routine assessment.
- Unknown/missing completion text maps to `Unknown`, never `Pending` by default.
- Hidden Units/SCOs remain visible as `NotOpen` observations rather than being
  silently deleted. They still advertise the current donor's execution,
  verification and duration capabilities; visibility is not a capability ban.
- The normalized completion observation remains independent from derived
  `NotOpen`: Fanyuchang completion/duration and YZBRH duration keep hidden
  and completed rows, while YZBRH and Auto_WeLearn completion plans admit
  only rows matching the donor's broad raw `iscomplete` contains `未` branch
  (`Pending`) and skip other `Unknown`, `InProgress` and `Completed` rows.
  YZBRH/Auto visibility filtering uses raw SCO `isvisible=false`; a hidden Unit
  with a missing/true SCO field remains eligible while merged visibility stays
  `NotOpen`. Auto duration uses the same raw SCO rule.
- `iscomplete` alone never supplies duration.
- `learntime` alone never marks a task completed.
- CMI completion and decimal progress remain independent observations.
- Generic TaskProgress never conflates raw time with progress; the independent
  DurationRead accepts only donor-observed bounded canonical integer seconds
  and fails closed on every other grammar.
- Read-only Progress/DurationRead never starts a missing or malformed CMI.
  PreserveFresh DurationReport accepts only the exact audited YZBRH
  uninitialized marker as a start signal; valid no-CMI remains non-mutating
  and fails before keep instead of synthesizing donor defaults. Every other
  malformed baseline and every malformed post-start read fails closed.
- A missing, string-valued or non-zero outer CMI `ret` fails closed before the
  nested `comment` document can be treated as an observation. Its attached
  `UnknownResultShape` contains only fixed labels, JSON types and a bounded
  result-state category.
- Missing/non-array Course `clist` and Unit/SCO `info` fail closed with exact
  `FieldDrift` shapes containing no source values or unknown object members.
- A post-start baseline or final readback without the complete preservation set
  fails closed; missing facts are never replaced with mutation defaults.
- Unknown fields are dropped from sanitized normalized data. The normalized
  `welearn.sco.v2` fixture retains only bounded response-order, Unit/SCO
  visibility and completion-observation facts needed by donor batch planning;
  the derived `visible`/`NotOpen` state remains separate.
- Fresh detail re-lists the Course and exact SCO instead of echoing a persisted
  scan payload; malformed or disappeared identities fail closed. Its public
  normalized Task and nested detail Task must remain value-identical;
  mixed injected/restored snapshots fail before execution rebind.
- Every normalized Task fingerprint uses an explicit version prefix and is
  recomputed at execution rebind; a stale fingerprint cannot authorize changed
  normalized facts even when the public and nested views agree.
- Fresh execution rebind reprojects public `remote_state` from normalized
  visibility and completion observations; hidden Tasks must be `NotOpen`, and
  every contradictory public state fails before transport.
- Fresh SCOs retain exactly the five inventory-owned read, resource execution,
  verification and duration capabilities. Rebind rejects missing, duplicate or
  unevidenced extra capabilities even when the current step needs only a subset.
- Duration reporting preserves completion, progress, score and success status;
  any drift rejects the entire execution outcome.
- A successful mutation response is insufficient unless fresh CMI changes a raw
  time observation.
- Authentication failure after a mutation never replays the lifecycle.
- ResourceExecution requires fresh identity rebind, exact completion/progress/
  selected-score readback and goal-bound recovery with no mutation replay.
- ResourceExecution exposes ordinary zero-time and donor fresh-time CMI facts
  separately. Atomic-final plans and native encoding retain both time fields,
  while singleton TaskExecution rejects them before transport until shared
  one-start authority exists; read-only recovery verification remains enabled.
- ResourceExecution accepts the exact uninitialized marker for zero-time or
  save-only completion, but fresh-time mode rejects it before start because no
  real session/total-time fields exist to preserve.
- ResourceExecution can emit either plain JSON or the exact donor
  `[INTERACTIONINFO]` CMI suffix without changing the underlying document.
- ResourceExecution distinguishes set-then-save from the modular Auto
  completion-bearing save-only tuple and treats
  per-call acceptance as a receipt, never the success predicate; an explicit
  rejection may continue to the donor's next write, while ambiguity stops.
- Transport documents must expose exactly the frozen start/set/save receipt
  slots. Missing or extra slots fail before CMI parsing, but present `false`
  receipts remain diagnostic and can pass only through exact fresh readback.
- ResourceExecution freezes the current full/simple-Referer or historical
  minimal/task-Referer mutation profile, applies its endpoint and Referer to
  baseline/start/set/save/verification, and never combines profile halves into
  an unevidenced request.
- Resource execution plans accept only the four complete audited current,
  legacy and atomic-final wire profiles. Cross-donor combinations fail both at
  TaskExecution preparation and the native transport boundary before I/O.
- Atomic duration-completion plan tests derive each complete profile from one
  profile discriminator and frozen target, accept both evidenced zero-second
  shapes, enforce the shared 19,800-second ceiling, and
  reject restored profile/cadence/protocol/score/CMI/write-family drift. These
  are pure plan tests and do not claim persistence, scheduling or mutation
  authority.
- Atomic combined-document tests keep CMI evidence separate from ordered
  mutation receipts. Current-donor fixtures require post-duration CMI, an
  accepted start, one set/save slot and either every keep or one terminal
  rejection. A current zero-target fixture proves start plus fresh CMI and
  set/save are still required with an empty keep list. Auto fixtures require
  exactly one keep per full minute, no post-duration/set slot and accept an
  empty zero-target keep list. Explicit
  `false` receipts remain valid diagnostics, while missing, extra or
  post-rejection mutations fail before CMI parsing. No execution entry is
  fixture-claimed by these value tests.
- Atomic child-plan tests materialize current Fanyuchang only with an explicit
  frozen bounded deterministic target including zero, take modular Auto targets
  only from the exact validated batch entry, preserve Auto's zero equal-floor child, and
  reject every singleton flow. Bounded JSON round trips retain Course/Task and
  ordinal bindings; unknown fields, version drift, cross-flow profiles,
  missing/extra target authority and oversized artifacts fail closed.
- Core-artifact adapter tests freeze `welearn.atomic-child.v1`, require provider
  `welearn`, prove the JSON payload contains only the eight bounded plan fields,
  preserve both profiles' zero target, and verify Debug redaction. Foreign
  provider or type, another valid ordinal, wrong frozen target and drifted batch facts all
  fail before a child plan can be restored.
- Prepared-child recovery tests require the independently durable complete
  batch, expected ordinal, frozen Fanyuchang target and exact Core artifact to
  rebind together. A different ordinal/target or drifted batch cannot rebuild
  the coordinator input, and successful restoration still performs no I/O.
- Fresh atomic-planning fixtures require explicit parent Course, atomic flow,
  ordered Unit selection, expected child and exact target authority. They prove
  reversed Fanyuchang Unit order is preserved, a complete 61-child Auto
  membership yields and retains zero-second children, and singleton flows,
  duplicate/missing selection, missing target, unselected child or disappeared
  child cannot be guessed into a plan.
- Prepared atomic-executor fixtures prove one complete fresh TaskDetail rebind
  occurs before exactly one transport call and exact final CMI verification.
  A foreign fresh child stops before transport; successful Auto and Fanyuchang
  fixtures return only the frozen profile/target, time proof and sanitized
  receipt diagnostics. They also prove the exact conditional sequence is
  prepared before transport and that post-mutation verification-persistence
  failure remains HumanRequired rather than being confused with a missing sink.
- Atomic sequence fixtures bind `welearn.atomic-duration-completion.v1` to the
  exact child artifact. Fanyuchang freezes accepted start, a zero-or-target
  terminal-rejection keep phase, phase-3 observation-gated set and final save;
  Auto freezes start, exactly `floor(target / 60)` keeps and final save. Zero
  targets retain the donor-distinct empty keep phases, and a foreign child
  artifact fails before sequence preparation.
- Native atomic transport tests bind current duration/completion to the
  query-uid endpoint and simple Referer, while Auto binds its duration phase to
  plain endpoint/simple Referer and switches only final completion to the
  task-specific Referer. Renewal-stage tests allow Authentication renewal only
  before start and during final read-only verification; every error after the
  first mutation maps to non-retryable HumanRequired. A safe mutation-result
  observation survives that mapping without becoming a receipt or replay
  authority. The transport trait and
  native implementation remain unregistered and unreachable from singleton
  TaskExecution.
- Pure atomic verification tests require exact completed/progress-100/profile
  score CMI, prove current-donor post-duration session/total time is preserved,
  and reject score or time drift as RemoteChanged. Auto verification accepts
  arbitrary observed time while requiring score 0 because its save-only final
  does not write a zero-time CMI payload. False mutation receipts remain
  diagnostic when the independent fresh readback proves the exact goal.
- Atomic initial-CMI tests accept initialized CMI, explicit no-CMI and the
  audited uninitialized marker, but reject any other document before durable
  issue/start. The combined document constructor repeats the same check so an
  injected transport cannot bypass the pre-mutation protocol gate.
- Shared atomic sink-value tests freeze the generic ordinal/digest bounds and
  Debug redaction, while WELearn tests freeze its five operation type strings.
  Native durable-boundary tests prove exact `issue → send → receipt` ordering
  across successive ordinals, persist explicit false receipts before
  continuing, reject a missing injected sink, send nothing after an issue
  failure, record no receipt for an ambiguous response, and map receipt
  persistence failure to non-replayable HumanRequired. No Storage type enters
  the Provider fixture boundary.
- Atomic digest tests prove deterministic domain-separated request/response
  hashes, bind kind/ordinal/endpoint/Referer and ordered form fields, distinguish
  ambiguous concatenation boundaries and reordered forms, reject Cookie form
  input, and show issue/document Debug cannot expose route, action, session or
  response text. Response hashing accepts only the bounded native document
  wrapper, and native requests use those hashes for the shared issue/receipt
  boundary.
- Exact atomic-verifier tests derive the final save ordinal from the complete
  receipt sequence and return a separate domain-separated observation digest
  only after fresh CMI proof. A changed but still valid fresh observation
  changes the digest; Debug remains hash-redacted. The proof also exposes the
  final receipt acceptance bit without treating a negative receipt as verified.
- Prepared coordinator tests record exactly one Core verification for an
  accepted final Fanyuchang save after fresh proof, bind it to the derived save
  ordinal and never record one for Auto's explicit negative final receipt. A
  missing durable sink after an accepted save returns non-retryable
  `HumanRequired` after one transport call rather than replaying the lifecycle.
- Runtime settings retain the current donor's one-second heartbeat and short
  session behavior, expose only the audited 1/60-second heartbeat values, and
  bind each cadence to its exact donor wire mode.
- Duration plan tests reject out-of-range targets and every protocol/cadence
  mismatch at TaskExecution and native entry before fresh detail or session I/O.
- The preservation-mode schedule sends an initial keep and one keep per full
  interval, while a trailing partial interval adds elapsed time but no
  unevidenced final keep.
- Client-counter phase mode always starts and sends exactly one paired counter
  keep per real second. Singleton implicit-server matches Auto single-file: it
  starts, sends no initial keep, uses the 60-second cadence and ends bare.
- Preservation mode retains YZBRH's two-second pre-read/start/first-keep/final
  gaps outside the requested duration, omits current-only keep fields, writes
  literal final `status=unknown`, and exposes accepted/rejected receipt counts.
- Well-formed negative historical duration receipts may continue and can only
  succeed after exact fresh preservation/readback; malformed or network-
  ambiguous receipts still stop with no replay.
- Duration document tests bind start receipt presence, accepted/rejected keep
  totals and final receipt presence to each frozen protocol. False receipts are
  valid slots; missing, extra or impossible slots fail before CMI parsing.
- Completion `auto` tests freeze fresh-time versus zero-time facts and reject a
  malformed plan binding before Provider transport; immutable plan context is
  not authority to execute the atomic duration-completion flow, and both exact
  atomic-final shapes fail closed at singleton TaskExecution.
- Batch tests mark Fanyuchang duration and modular Auto duration as
  `AtomicDurationCompletion`, require both child capabilities and keep YZBRH/
  Auto single-file singleton duration separate.
- Batch child rebind tests use a complete fresh TaskDetail, preserve frozen
  dispatch when a SCO response ordinal moves, allow an already-completed child
  to enter no-replay preflight, reject selected-Unit drift, and re-apply only
  the visibility rule used by the selected donor flow.
- Complete batch snapshot tests round-trip all seven donor flows under the
  namespaced `welearn.batch-plan.v1` schema, cover the valid 8,192-child maximum
  within the independent 8 MiB bound, and reject empty/oversized input, unknown
  fields, version drift, dispatch drift and Auto child-target drift. Decode
  always re-enters full frozen-plan integrity validation.
- Durable atomic-child recovery tests decode the parent authority and complete
  batch snapshot together, accept both Fanyuchang and Auto target shapes, and
  reject cross-attempt Course, flow, Unit selection, aggregate, expected-child
  and child-artifact substitutions before fresh I/O.
- Atomic recovery evidence tests round-trip the bounded hash-only Fanyuchang
  pre-final observation, reject schema/zero-digest/child/time substitution,
  preserve the terminal-rejected-keep ordinal shape, adapt only the exact Core
  phase-3/type/digest observation, and verify Auto from its deterministic
  receipts without inventing a pre-final time observation.
- Generic sequence-record recovery tests require the exact child-bound plan,
  equal issue/receipt records, contiguous ordinals and donor-specific operation
  order. They recover Fanyuchang's early-rejection final ordinal and Auto's
  deterministic floor count, while rejecting foreign plans, mislabeled keeps
  and ordinal drift before fresh-CMI proof.
- Prepared recovery-coordinator fixtures prove sequence-record drift stops
  before Task discovery, fresh Task drift stops before final CMI, and a valid
  Fanyuchang attempt performs exactly one detail rebind followed by one
  read-only final fetch. Transport-profile tests keep current Fanyuchang's
  full/simple-Referer read separate from Auto's legacy minimal/task-Referer read.
- Provider/account concurrency accepts the Auto_WeLearn donor's bounded
  1–100 worker setting while preserving conservative defaults.
- Hidden SCO execution rebinds the same Course/SCO and fresh visibility
  observation before mutation; it never rewrites `NotOpen` as an open state.
- Capture-assisted and external-browser-OAuth Cookie credentials must originate
  from CaptureTool or a BrowserExtension and pass the same authenticated
  Course-read validation; their immutable recipe versions/methods stay distinct.
- The Capture recipe must keep both WELearn and SSO origins explicit, start at
  the audited prelogin route, and emit only the final WELearn Cookie; browser
  captcha/SMS route material never enters fixtures or credentials. QQ, WeChat
  and Apple may add only the three exact public-audited navigation origins;
  their headers, storage and Cookies never become credential sources.
- A fixture snapshot containing only anonymous prelogin Cookies plus the exact
  Course-list request demonstrates that request observation alone is
  insufficient: the response can be `200 text/html`. Recipe v4 requires `200
  application/json` under the current loader, and native validation rejects
  the login-script body independently.
