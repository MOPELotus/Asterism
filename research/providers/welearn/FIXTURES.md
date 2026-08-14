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

The duration pair is a synthetic before/after readback. It keeps completion,
progress, score and success status identical while changing only opaque raw time
observations. Injected lifecycle tests freeze provider/task runtime overrides,
reject completion/score drift, reject absent/partial preservation fields and
reject a final read with no time change.
Native-boundary tests separately reject malformed or unsupported mutation result
codes and prove the preserved CMI form state. Wire-plan regressions keep
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
responses remain covered by HumanRequired no-replay tests.

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
  nested `comment` document can be treated as an observation.
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
