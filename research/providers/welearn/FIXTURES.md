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

The Auth fixtures cover bounded response classification, strict callback
allowlisting and `HumanRequired` separation without retaining callback/token
material. The SMS fixture uses the audited `code=0 + extraCheck.vcToken`
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
codes and prove the preserved CMI form state.

The ResourceExecution fixtures prove the independently written bounded CMI
preset and the nested fresh-read goal used by immediate verification and crash
recovery. Tests require the exact selected completion/progress/score tuple,
prove fixed and bounded-random settings stay stable for the frozen Task, and
prove recovery calls only the non-mutating CMI reader.

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
- `iscomplete` alone never supplies duration.
- `learntime` alone never marks a task completed.
- CMI completion and decimal progress remain independent observations.
- Generic TaskProgress never conflates raw time with progress; the independent
  DurationRead accepts only donor-observed bounded canonical integer seconds
  and fails closed on every other grammar.
- Missing or malformed CMI never triggers a start/mutation fallback.
- A missing, string-valued or non-zero outer CMI `ret` fails closed before the
  nested `comment` document can be treated as an observation.
- A post-start baseline or final readback without the complete preservation set
  fails closed; missing facts are never replaced with mutation defaults.
- Unknown fields are dropped from sanitized normalized data.
- Fresh detail re-lists the Course and exact SCO instead of echoing a persisted
  scan payload; malformed or disappeared identities fail closed.
- Every normalized Task fingerprint uses an explicit version prefix.
- Duration reporting preserves completion, progress, score and success status;
  any drift rejects the entire execution outcome.
- A successful mutation response is insufficient unless fresh CMI changes a raw
  time observation.
- Authentication failure after a mutation never replays the lifecycle.
- ResourceExecution requires fresh identity rebind, exact completion/progress/
  selected-score readback and goal-bound recovery with no mutation replay.
- Runtime settings retain the current donor's one-second heartbeat and short
  session behavior inside explicit bounded ranges.
- The preservation-mode schedule sends an initial keep and one keep per full
  interval, while a trailing partial interval adds elapsed time but no
  unevidenced final keep.
- Provider/account concurrency accepts the Auto_WeLearn donor's bounded
  1–100 worker setting while preserving conservative defaults.
- Hidden SCO execution rebinds the same Course/SCO and fresh visibility
  observation before mutation; it never rewrites `NotOpen` as an open state.
- Capture-assisted Cookie credentials must originate from CaptureTool or a
  BrowserExtension and pass the same authenticated Course-read validation.
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
