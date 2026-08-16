# Cidaren upstream checks

Cidaren donor revisions are reproducible audit snapshots, not permanent pins.
Before and after each meaningful Provider checkpoint, re-read both donors'
default-branch refs, tags/releases and intervening commits. If a ref changes,
record the old and new revision plus a bounded diff summary here before
updating `SOURCES.md`, `DONOR_DIFFERENCES.md`, protocol code or tests.

## 2026-08-13 reopened-source audit

| Upstream | Default branch snapshot | Tags/releases | Change assessment |
|---|---|---|---|
| `ularch/Easy_Cidaren` | `master@bce9559f536ebbdad791f41ed4e111b30accb05d` | Latest tag/release remains `1.5.4@7e29ee43692f4c0807fae8cf7f74a5a674793097`, published 2025-12-09 | Compared with the former project snapshot `f2b5c25`: `c8de5f4` reopens executable source and adds token-only local Capture, public `jv=3_2265/3_2277` plus an alternate `3_1021` transform, progress UI and a post-run score read; `bce9559` documents reopening. Platform call sites otherwise remain the known token/account, class/study inventory, word evidence and five mutation routes |
| `MOPELotus/Easy_Cidaren` | `master@a74b4a2c1cdc5d38f568d41ccb11bb4d441ee4f1` | No published GitHub release at check time | No branch movement since the existing 2026-05-13 snapshot. It remains the additive Composite Capture, current encrypted word lookup and authenticated `jv=99` evidence line |

The latest ularch release asset is historical relative to its reopened source:
`Easy_Cidaren1_5_4.zip`, 103462537 bytes, SHA-256
`526011a4ccd14cc38887a663d54a5c78c33d8ea3d48e6c424a32128b6d0d8aca`.
The asset was identified from GitHub release metadata and is not redistributed
or treated as newer than `master`.

### Incremental implementation result

- added exact bounded public `jv=3_2265` and `jv=3_2277` decoders;
- retained both divergent exact `3_1021` transforms with unique/equal-result
  acceptance and ambiguity rejection;
- retained MOPE/current `jv=99` HKDF/AES-GCM instead of falling back to the
  public legacy decoder;
- accepted public-donor token-only Capture at the Provider validation and
  stored-session boundaries and implemented its distinct request-header
  recipe, while retaining Composite Capture and native OAuth material;
- retained the post-run score as an independent fresh verification fact;
- integrated the ordered multi-recipe Core contract as soon as it became
  available; actual local helper execution and executable BrowserBridge remain
  shared gaps rather than reasons to drop either donor path.

Post-implementation ref check: `ularch/master` remained `bce9559`,
`MOPELotus/master` remained `a74b4a2`, ularch latest release remained `1.5.4`
and MOPELotus still exposed no GitHub release. No additional diff entered this
checkpoint between the initial audit and the 99-test Provider verification.

### 2026-08-13 exact task-score checkpoint

Pre-implementation ref check again resolved `ularch/master` to full revision
`bce9559f536ebbdad791f41ed4e111b30accb05d` and `MOPELotus/master` to
`a74b4a2c1cdc5d38f568d41ccb11bb4d441ee4f1`. The latest ularch release/tag
remained `1.5.4@7e29ee43692f4c0807fae8cf7f74a5a674793097`, and MOPELotus still
published no GitHub release. There was therefore no upstream diff to port
before this checkpoint.

The reopened public donor's new `main_api.get_task_score` was then audited as
its own protocol route rather than approximated from PageTask alone. Asterism
now implements exact `ClassTask/Info` and `StudyTask/Info` query shapes,
account-bound read headers, bounded plain/legacy/current decoding,
conflict-checked `score/task_score/grade`, precise thousandth-point conversion
and SubmissionVerify integration. The public donor's dedicated requests
session does not copy `UserToken`; that is treated as a donor implementation
defect, not a platform requirement. Its study query also omits stable
`list_id`, so a fresh `task_id=-1` unit retains its identity-bound
`StudyTask/List` score rather than issuing an ambiguous request.

Post-implementation fetch resolved the same two full revisions and the same
`1.5.4` tag target; no new donor diff entered while the 105-test Provider,
all-target clippy and scoped diff checks completed.

### 2026-08-13 persisted native-OAuth validation checkpoint

The pre-implementation fetch again resolved `ularch/master@bce9559` and
`MOPELotus/master@a74b4a2`, with ularch release/tag `1.5.4` unchanged. The
OAuth V2 handoff documents remained the same imported manifest. No donor or
handoff delta preceded this incremental authentication audit.

The audit found that initial and Core pre-commit NativeProviderLogin checks
used the current OAuth V2 User-Agent/derived `Abc`/`Authorization-v: 00`
context, while a later stored-session validation discarded the non-secret
acquisition distinction and fell back to legacy donor headers. The resolved
Cidaren session now retains only that validation-context enum alongside its
zeroizing token/crypto material; NativeProviderLogin reuses the current OAuth
readback path, while manual and Capture origins remain on their evidenced
donor-header path.

Post-implementation fetch retained the same two donor revisions and ularch
tag target. The 106-test Provider suite, all-target clippy and scoped diff
checks completed without a new upstream delta.

### 2026-08-13 runtime-timing parity checkpoint

The pre-implementation fetch again retained `ularch/master@bce9559`,
`MOPELotus/master@a74b4a2` and ularch tag/release `1.5.4`. Reopened donor
`config/config.json` defaults `min_time=max_time=2`; `PublicInfo` replaces any
lower value with 2, and both settings controls also enforce a minimum of 2.
The Asterism schema had incorrectly defaulted and allowed both values at zero.

Runtime schema revision 2 now freezes the evidenced 2-second defaults and
minimum while preserving Provider/account/Task overrides, range validation and
stable step-bound selection. The independently evidenced reported-answer and
skip duration settings are unchanged.

Post-implementation fetch retained both donor revisions and the same ularch
tag target. The 106-test Provider suite, all-target clippy and scoped diff
checks passed without an intervening upstream delta.

### 2026-08-13 attempt-residence timing checkpoint

After one transient TLS fetch failure was retried successfully, the
pre-implementation refs still resolved to `ularch/master@bce9559`,
`MOPELotus/master@a74b4a2` and ularch tag/release `1.5.4`. Public donor
`answer_questions.submit` waits 1-2 seconds between the last `VerifyAnswer`
and `SubmitAnswerAndSave`; `jump_read` waits 1-3 seconds before advancing a
reading card. These are distinct from configurable `min_time/max_time` after a
completed step.

The Provider-private issued command now carries a stable bounded delay before
advance execution: 1-2 seconds for a verified Question and 1-3 seconds for a
reading card. Start, Verify, Skip and word-selection mutations remain
immediate. This gives future durable Core integration an explicit scheduling
fact instead of either sleeping inside Provider code or collapsing all donor
timing into one setting.

Post-implementation fetch of both remotes retained the same revisions and
ularch tag target. The 106-test Provider suite, all-target clippy and scoped
diff checks passed without an intervening donor delta.

### 2026-08-13 current-attempt-progress checkpoint

The pre-implementation fetch resolved `ularch/master@bce9559f536ebbdad791f41ed4e111b30accb05d`
and `MOPELotus/master@a74b4a2c1cdc5d38f568d41ccb11bb4d441ee4f1`. The latest
ularch tag/release remained `1.5.4@7e29ee43692f4c0807fae8cf7f74a5a674793097`,
and neither default branch had a new commit or release delta.

The reopened public payload publishes optional `topic_done_num` and
`topic_total` counters on both ordinary question and mode-0 reading-card
steps. The Provider now parses them as bounded, conflict-checked remote
observations, exposes them from the private attempt flow, and keeps them
separate from local position, Question identity and mutation routing. Missing
or zero pairs remain valid legacy payloads; inconsistent, fractional, negative,
oversized or inverted counters fail closed as protocol drift.

Post-implementation ref checks retained both revisions and the same ularch
release. The 107-test Provider suite, all-target clippy and scoped diff checks
passed without an intervening upstream delta.

The full `ularch/Easy_Cidaren` source tree was also re-read at this checkpoint.
Its executable platform surface remains the already mapped inventory,
word-evidence, five assessment mutations, task-score read, token-only proxy
Capture and UI-only helper paths; no new platform route or crypto transform
was found. `MOPELotus/Easy_Cidaren` was re-read in parallel and still only adds
the known `crypto.json` capture writer and `jv=99` context around that same
surface. No additional Provider capability delta entered this checkpoint.

### 2026-08-13 BrowserBridge Capture command checkpoint

The pre-implementation ref check again resolved `ularch/master` to
`bce9559f536ebbdad791f41ed4e111b30accb05d` and `MOPELotus/master` to
`a74b4a2c1cdc5d38f568d41ccb11bb4d441ee4f1`. The latest public tag/release
remained `1.5.4@7e29ee43692f4c0807fae8cf7f74a5a674793097`; MOPELotus still
published no release. A complete source-tree audit found no new platform route,
assessment operation, Capture field, or crypto transform beyond the mapped
token-only proxy recipe, Composite `CDR_LOGIN_INFO`/optional
`CDR_USER_SESSION`, and authenticated `jv=99` path. The donors do not expose a
separate browser action for Start/Verify/Submit; those operations remain native
HTTP mutations and their durable continuation is a Core Attempt contract.

Cidaren now has a Provider-private typed `CaptureSnapshot` command/result
boundary. Commands are recipe-versioned (`TokenOnly` v1 or `Composite` v2),
restricted to the audited `https://app.vocabgo.com` origin, and bound to Core's
session nonce, frame, stable remote Task identity and one-shot sequence. A
TokenOnly result requires only a bounded `UserToken` and rejects Composite
fields. A Composite result requires bounded object-shaped `login_info` that
successfully parses through the exact `jv=99` HKDF/AES-GCM context; optional
`CDR_USER_SESSION` is observed but not required. Result/event debug output
redacts values and owned strings are zeroized on drop. The transport document
is bounded and rejects unknown fields, foreign binding, arbitrary selectors or
scripts; it is a one-shot observation, not a persisted credential or receipt.

The shared Core BrowserBridge session boundary still needs to carry this typed
opaque command/result, persist issued sequence/result correlation and route the
validated snapshot through the Capture credential commit. Cidaren does not
serialize the snapshot into Domain state or claim executable helper completion.

Post-implementation refs retained both full donor revisions and the same
`1.5.4` release. The 110-test Provider suite, all-target clippy and scoped diff
checks passed without an upstream delta.

The follow-up fixture-binding checkpoint re-resolved `ularch/HEAD` and
`ularch/master` to `bce9559f536ebbdad791f41ed4e111b30accb05d`, tag `1.5.4` to
`7e29ee43692f4c0807fae8cf7f74a5a674793097`, and `MOPELotus/HEAD` plus
`master` to `a74b4a2c1cdc5d38f568d41ccb11bb4d441ee4f1`. No default-branch or
tag movement occurred. The two committed synthetic Capture result fixtures
now run through the same typed command/result parser; the 111-test Provider
suite, all-target clippy and scoped diff checks passed.

The next Provider-only registration checkpoint found no donor revision delta.
The existing native answer-evidence loader and fresh Task score verifier were
already complete and independent of remote mutation, so Cidaren now registers
`AnswerResolve` and `SubmissionVerify` publicly. Task rows advertise both
capabilities; the resolver binds fresh StudyTask/Course evidence and preserves
all audited fallback semantics, while verification remains a separate
completion/progress/score read and leaves each Question `Unverified`. The
public QuestionInventory/QuestionParse and SubmissionExecute slots were still
explicitly tied to the Main-owned durable Attempt/QuestionSession contract at
that checkpoint; the later pre-Question adapter checkpoint below supersedes
the QuestionInventory part without inventing a read-only QuestionParse route.

Post-registration refs remained `ularch/master@bce9559f536ebbdad791f41ed4e111b30accb05d`,
`MOPELotus/master@a74b4a2c1cdc5d38f568d41ccb11bb4d441ee4f1` and
`1.5.4@7e29ee43692f4c0807fae8cf7f74a5a674793097`. The 114-test Provider suite,
all-target clippy and scoped diff checks passed.

The BrowserBridge exchange follow-up re-read both default branches and the
public tag again; all three revisions remained unchanged. To match the new
Core durable exchange boundary, Cidaren now exposes stable command/result type
strings and SHA-256 digest helpers. The command digest covers validated
canonical typed envelope JSON; the result digest covers only the bounded raw
transport document. Core stores type/digest/sequence metadata, while Cidaren
retains and zeroizes the typed Capture values and performs the final credential
validation/commit. No donor protocol delta entered this checkpoint. The
Provider suite remained at 114 passing tests with all-target clippy and scoped
diff checks clean.

### 2026-08-14 durable Capture exchange adapter checkpoint

The new-day pre-implementation check resolved `MOPELotus/HEAD` and `master` to
`a74b4a2c1cdc5d38f568d41ccb11bb4d441ee4f1`, `ularch/HEAD` and `master` to
`bce9559f536ebbdad791f41ed4e111b30accb05d`, and historical
`github123666/main` to `1409858800f3c4bd27577a08049bf1f8d17a069c`.
The latest public tag remained
`1.5.4@7e29ee43692f4c0807fae8cf7f74a5a674793097`; no default branch, tag or
release delta entered the checkpoint. Fresh temporary source-tree audits of
both current donors found the same account, class/study inventory,
word-evidence, five assessment mutation, task-score and Capture call sites
already recorded in the capability map.

Core's durable BrowserBridge exchange ledger was then integrated at the
Provider edge without persisting a Cidaren payload. The Provider now returns
an immutable typed-command/issued-record pair whose nonce is derived from the
exact Core session ID and whose type, digest and bounded sequence cannot be
assembled independently. Result completion repeats fresh Task rebinding,
validates the exact typed result and returns its zeroizing Capture snapshot
separately from terminal hash metadata. Public helper payload dispatch and
Capture credential commit remain Main-owned shared gaps, not dropped donor
abilities. The Provider suite reached 116 passing tests with all-target
clippy clean.

The same unchanged-source checkpoint also closed the Provider-side credential
shape conversion. A validated token-only snapshot now consumes into one
zeroizing access-token replacement; a Composite snapshot consumes into an
atomic access-token/crypto-context pair. Optional `CDR_USER_SESSION` remains
an observation and is discarded, matching both donors' executable use. Core
still owns recipe/acquisition/account validation and the atomic SecretStore
commit. The Provider suite reached 117 passing tests with strict clippy clean.

The executable-call audit also rechecked the donor's shared response handler.
It accepts `code=20001` only when `data` is Python-truthy and accepts terminal
`code=20004`; the prior word-selection parser instead accepted empty `20001`
and rejected `20004`. The parser and one-shot flow now reject empty/null/zero/
false `20001` data, preserve terminal selection as a receipt and never issue
`StartAnswer` afterward. This is receipt handling, not verification. The
Provider suite reached 119 passing tests with strict clippy clean.

A follow-up on the same unchanged handler moved localized receipt-message
classification behind the numeric/data success gate. Previously a malformed
`code=0` response could copy the donor's completed/word-selection UI text and
become a receipt. Both assessment and word-selection parsers now reject that
shape; terminal text remains acknowledgement context only.

The issue-tracker refresh then found a necessary platform exception in public
issue 72: `StartAnswer` returned exactly `code=20001`, `msg=需要选词！`,
`data=null`; the maintainer marked it fixed in 1.5.2 even though the reopened
shared handler still describes truthy-data success. Asterism now recognizes
only that exact code/message tuple as typed `WordSelectionRequired`, while
unrelated empty `20001` responses remain rejected. If it appears after a word
selection submission, the attempt requires fresh Task/word-plan rediscovery
before another selection and cannot proceed to Start. The Provider suite
reached 120 passing tests with strict clippy clean.

The same issue refresh found public issue 99, where the current donor exits on
`topic_mode=73` and the maintainer confirms it is unimplemented. Its public
diagnostic attachment was read only in a temporary directory with values
redacted; the stable shape is two answers, two positive word lengths and no
options. A synthetic fixture now freezes only that structure. Asterism parses
it as a bounded FillBlank Question and can use the already evidenced Skip
operation, but does not guess the missing multi-answer Verify encoding. The
Provider suite reached 121 passing tests with strict clippy clean.

The durable-attempt checkpoint re-resolved `MOPELotus/master` at `a74b4a2`,
`ularch/master` at `bce9559`, its latest tag `1.5.4` at `7e29ee4`, and
`github123666/main` at `1409858`; no branch or tag delta was present. Both
current donors route `StartAnswer` through a helper that repeats the same GET
after receiving an unknown `jv`, and rerunning their task workflow calls
`StartAnswer` again. This is recorded as donor behavior after a definite
response, not evidence that a request with unknown transport outcome is safe
to replay. Asterism's Provider-private interface therefore retains ambiguous
no-replay while adding encrypted, Task-bound selection/start/reading-card
continuations and real-Question materialization bundles. Correlation remains
audit-only so recovery can reconstruct the same Provider/account/Task binding.
The Provider suite reached 128 passing tests before strict-clippy closure.

The public pre-Question adapter checkpoint re-resolved the same three donor
heads/tags with no movement. Provider API integration now preserves the exact
frozen operation type, request digest and donor delay before execution; accepts
selection/start/reading-card responses into encrypted continuations; directly
materializes the first real Question; and represents completion-before-first-
Question as a distinct terminal outcome. Synthetic adapter tests cover
`StartAnswer -> reading card -> SubmitAnswerAndSave -> Question`, repeated word
selection and terminal completion. No current donor exposes a safe ambiguity
readback, so recovery deliberately returns no outcome rather than replaying.

The post-materialization recovery audit found that phase plus rotated
`topic_code` was insufficient for a multi-relation matching Question: after a
crash it could not identify how many immutable Draft relations had already
been accepted. The encrypted Question artifact is therefore versioned as
`cidaren.question-attempt.v2` and adds only a bounded monotonic
`verified_steps` count. Provider recovery rebinds the fresh Task and complete
Question fingerprint, regenerates the deterministic wire relation sequence
from the immutable selected answer, removes the accepted prefix and rejects
every count/phase/selection disagreement. No new donor route or revision delta
was involved.

On 2026-08-14 the integration checkpoint again resolved
`MOPELotus/master` at `a74b4a2c1cdc5d38f568d41ccb11bb4d441ee4f1`,
`ularch/master` at `bce9559f536ebbdad791f41ed4e111b30accb05d`,
latest public tag `1.5.4` at `7e29ee43692f4c0807fae8cf7f74a5a674793097`
and historical `github123666/main` at
`1409858800f3c4bd27577a08049bf1f8d17a069c`; no branch or tag delta was
present. A complete Python call-site search across all three snapshots also
confirmed that `api/translate.py` has no importer or caller: modes 51-54 use
only the task word inventory and `word_examples`, so the dormant standalone
Google helper does not add an executable donor capability. Main-owned durable
Question-read orchestration now drives the registered mutation-backed adapter,
persists each frozen operation before execution and atomically materializes the
first real Question for the public task endpoint. All 135 Cidaren Provider
tests passed against that shared integration.

The same 2026-08-14 checkpoint refreshed the public issue and release feeds,
not only Git refs. No issue had changed after 2026-07-06. Issues 109 and 113
report only local proxy/Capture-helper interference; issue 112's maintainer
response identifies a ban or network failure without exposing a
device-binding route or algorithm; issue 108 is the already audited encoding
transition covered by the current public transforms and owner `jv=99` line.
The public GitHub Release feed does exist: latest `1.5.4` was published on
2025-12-09 and its `Easy_Cidaren1_5_4.zip` asset has SHA-256
`526011a4ccd14cc38887a663d54a5c78c33d8ea3d48e6c424a32128b6d0d8aca`.
This corrects the prior tag-only description but introduces no newer protocol
or capability delta. The reports likewise retain the existing shared
helper-execution and live-validation gates.

The subsequent 2026-08-14 Provider-hardening checkpoint again resolved
`MOPELotus/master@a74b4a2c1cdc5d38f568d41ccb11bb4d441ee4f1`,
`ularch/master@bce9559f536ebbdad791f41ed4e111b30accb05d`, public
`1.5.4@7e29ee43692f4c0807fae8cf7f74a5a674793097` and historical
`github123666/main@1409858800f3c4bd27577a08049bf1f8d17a069c`. Historical
latest Release remained `v3.73` from 2024-04-02; owner release metadata
remained unavailable. The public issue ordering remained headed by issue 112
at 2026-07-06, so no new route, crypto transform, authentication flow or
assessment operation entered scope.

With no upstream behavior delta, the checkpoint tightened only Provider-owned
secret/evidence lifetimes. Capture command digest bytes containing Core's
session nonce are cleared immediately after hashing. Partial word inventories,
case-folded keys, prototype results, aliases and completion prefixes now use
RAII zeroization through duplicate, identity-drift and asynchronous transport
failure paths; candidate validation borrows the immutable Question before any
owned copy. All 137 Cidaren Provider tests, all-target strict clippy, package
formatting and scoped diff checks passed.

The public 1.5.4 Release asset was then audited rather than treating its tag as
a sufficient proxy. `Easy_Cidaren1_5_4.zip` is 103,462,537 bytes and its local
SHA-256 exactly matched the GitHub asset digest
`526011a4ccd14cc38887a663d54a5c78c33d8ea3d48e6c424a32128b6d0d8aca`.
It is a Python 3.12 PyInstaller bundle. The executable was not run; its outer
CArchive and embedded PYZ were enumerated and their donor-owned code objects
were read with a version-matched archive reader.

The packaged token helper differs from the current default branch only by
using direct console output where master now uses the donor logger. Proxy
save/restore, certificate import, request-header `UserToken` observation and
token-file output are otherwise the same. The executable's learning-platform
surface contains the already mapped `Student/Main`, `StudyTask/List`,
`StudyTask/Info`, Course-page/SearchWord/StudyWordInfo reads,
`ClassTask/PageTask`, task score reads, five assessment mutations and legacy
`jv` transforms. No additional platform origin, route, authentication flow,
Capture field or mutation appeared.

Two packaged modules are absent from the 1.5.4 tag source:
`api.cdr_status` and its UUID helper. Bytecode inspection confirmed that login
checks a donor-operated `https://cdr.660916.xyz/block` UUID list, while task
success/failure posts task name, result, duration and score to `/commit` with
fail-open telemetry errors. This is an external donor-operator
telemetry/device-block policy, not a Cidaren learning-platform capability. It
is recorded but not mapped into Provider API. Conversely, tag source contains
the standalone `api.translate` helper but the packaged executable omits it;
there is still no executable donor translation capability to port. This
release-asset delta therefore required documentation, not Provider behavior.

The same checkpoint separately audited the built-in token helper added on the
reopened public default branch after release `1.5.4`. `TokenCaptureService`
clears `fetch_token/token.txt`, preserves the current Windows system-proxy
configuration, enables fixed `127.0.0.1:8888`, starts
`mitmdump -s addon.py --listen-port 8888 --quiet --set
console_eventlog_verbosity=error`, and polls the token file every 500 ms. It
returns only the first observed `UserToken`; success, explicit stop and error
all terminate the child, wait up to three seconds before killing it, and
restore the old proxy. Although FiddlerCore binaries are present in the donor
tree, the current helper does not import or execute them. This refines the
shared helper-dispatch and cleanup requirements without adding another
Provider credential shape, Capture field or platform capability.

The post-helper hardening ref check again resolved
`ularch/master@bce9559f536ebbdad791f41ed4e111b30accb05d`, public
`1.5.4@7e29ee43692f4c0807fae8cf7f74a5a674793097`,
`MOPELotus/master@a74b4a2c1cdc5d38f568d41ccb11bb4d441ee4f1` and historical
`github123666/main@1409858800f3c4bd27577a08049bf1f8d17a069c`; no revision
delta entered the checkpoint. The Provider's high-level Capture result now
consumes an owned, bounded and Debug-redacted raw document so the token/crypto
JSON transport copy is zeroized after validation, hashing or any error path.
All 138 Cidaren tests and all-target strict clippy passed.

After Core added atomic read-only Question artifact materialization and a
session-aware AnswerResolve hook, Cidaren bound that hook to its existing
`cidaren.question-attempt.v2` artifact instead of ignoring the decrypted
continuation. The Provider requires one initial current Question at revision
1, validates type/phase/digest and repeats every Task/Question/fingerprint
check before loading answer evidence. No donor revision or protocol operation
changed; this closes a shared-integration binding gap. All 139 Cidaren tests
and all-target strict clippy passed.

The post-materialization execution checkpoint again resolved
`ularch/master@bce9559f536ebbdad791f41ed4e111b30accb05d`, public
`1.5.4@7e29ee43692f4c0807fae8cf7f74a5a674793097`,
`MOPELotus/master@a74b4a2c1cdc5d38f568d41ccb11bb4d441ee4f1` and historical
`github123666/main@1409858800f3c4bd27577a08049bf1f8d17a069c`; no upstream
delta entered the work. Cidaren now advertises a session-aware
SubmissionExecute slot, persists explicit Skip as a command intent, and maps
accepted operations to same-Question continuation, intervening pre-Question
continuation, next-Question materialization or terminal receipt without
replay or fabricated artifacts. Fixture coverage includes mode-73 Skip to
terminal completion, Verify/advance to a next Question, and a post-Question
reading-card transition. A mid-attempt word-reselection recovery fixture also
proves that the bounded next position survives both selection and StartAnswer
artifact rotations instead of regressing to the first Question. A separate
durable matching fixture restores the same immutable Draft through two token
rotations before terminal advance. Provider-side BrowserBridge preparation now
also freezes a zeroizing command artifact and proves exact recovery rebinding
without trusting helper echoes. A recovered-result adapter now reconstructs
command authority from only that encrypted artifact plus the persisted Issued
exchange before accepting a helper result. The completed adapter has no
snapshot-only consuming path and can hand Core an exact credential
replacement + terminal exchange pair for its atomic commit. Exact compact
TokenOnly/Composite command fixtures now freeze the same bytes used by helper
dispatch, encrypted recovery and the command digest. Recovered commands have
a separate 4 KiB symmetric issue/recovery bound and cannot consume the 256 KiB
helper-result budget or be issued in a form that recovery would reject.
All 149 Cidaren
tests, all-target strict clippy, formatting and scoped diff checks passed
against the landed atomic shared transition contract. The three symbolic
default branches, complete public tag refs and latest GitHub release metadata
were refreshed again after that fix.
Ularch remains on release `1.5.4` with the same asset size/digest, MOPELotus
still has no release, and the historical donor remains on `v3.73` from
2024-04-02. The complete Ularch issue listing was also refreshed; issue 112
from 2026-07-06 remains the most recently updated item and no newer route,
crypto, authentication or task behavior was reported. No revision, tag,
release or issue-evidence delta entered this checkpoint.

### 2026-08-14 post-terminal remaining-capability audit

Pre- and post-audit symbolic ref checks both resolved
`ularch/master@bce9559f536ebbdad791f41ed4e111b30accb05d`,
`MOPELotus/master@a74b4a2c1cdc5d38f568d41ccb11bb4d441ee4f1` and
historical `github123666/main@1409858800f3c4bd27577a08049bf1f8d17a069c`.
Public tag `1.5.4` still targets
`7e29ee43692f4c0807fae8cf7f74a5a674793097`; GitHub still reports it as the
latest Release, published 2025-12-09, with the same 103,462,537-byte
`Easy_Cidaren1_5_4.zip`. `MOPELotus` still exposes no GitHub Release. The
complete public issue feed remains headed by issue 112, last updated
2026-07-06. No commit, tag, release, issue, route, response transform, Capture
field or assessment-operation delta entered this checkpoint.

The remaining-capability audit was repeated after Core's credential terminal
path landed. The shared `asterism-capture` runner now consumes Cidaren's opaque
command owner, validates its exact dispatch binding, reads only the projected
request-header/local/session-storage sources and emits the typed terminal
artifact. Engine/Core now persist and atomically accept that credential result.
The earlier generic-helper-dispatch gap is therefore closed and must not be
reported as unfinished Provider work.

No evidenced donor call site remains both unimplemented and solvable in the
Cidaren crate. Four concrete gates remain:

- `CIDAREN-BLOCKER-BROWSER-CONTEXT`: the shared runner deliberately creates a
  temporary isolated Chromium profile. It cannot inherit the owner donor's
  authenticated PC WeChat XWeb storage, and it does not implement the public
  donor's Windows system-proxy, certificate and `mitmdump` lifecycle. This
  requires an authorized login in the isolated document or shared environment
  support for attach/proxy/XWeb acquisition, followed by live validation; no
  further Cidaren source enum, command, result or credential code can supply
  that external state.
- `CIDAREN-BLOCKER-LIVE-ACCOUNT`: every audited read/parser/crypto path has
  synthetic coverage, but current account, inventory/status vocabulary,
  duration, score and `jv=99` behavior require a supplied real account and
  sanitized observations. Provider metadata remains Development.
- `CIDAREN-BLOCKER-LIVE-MUTATION`: production validation of the five implemented
  mutations changes remote learning state and therefore requires explicit
  authorization plus an eligible Task. Ambiguous no-replay cannot be weakened
  to manufacture offline validation.
- `CIDAREN-BLOCKER-EVIDENCE-73`: issue 99 and unchanged donor code expose the
  multi-blank payload but explicitly do not implement its direct answer wire
  encoding. The evidenced Skip path is implemented. A new upstream commit or
  authorized trace is required before Answer execution can expand.

No Cidaren-specific Core contract blocker remains. The audited donors expose
neither a refresh endpoint nor per-Question answer-history readback, and the
packaged telemetry/device-block modules remain non-platform behavior. Those
absences are evidence boundaries, not Provider features to invent. This
checkpoint therefore updates the audit/blocker record rather than duplicating
the completed Capture credential path or adding speculative code.

### 2026-08-14 mandatory one-time full upstream sweep

This checkpoint applied the additional `AGENTS.md` and
`research/providers/NEXT_CHECKPOINT.md` requirement. It did not start from the
previous pins or limit review to later commits. Fresh full clones were used to
enumerate all three Git donor trees, READMEs/configs, every Python call site,
all branches and tags, all GitHub Releases, all 105 public non-PR issues and
comments, inline payload examples, screenshots and the already hash-verified
1.5.4 package inventory. All imported H5/capture/XWeb/OAuth handoff manifests
and documents were reread. The reproducible ledger is
`FULL_UPSTREAM_SWEEP.md`.

The sweep confirmed the same refs and release heads recorded above. Across all
14 public tags and 10 historical tags, the executable platform surface
converges on the already mapped Student/Main, class/study inventory/detail,
CoursePage/SearchWord/StudyWordInfo evidence and five assessment mutations.
Historical `v1.0/v1.1` contains a `get_all_task` symbol mentioning
`/Student/ClassTask/PageTask`, but its body makes no request and it has no
caller; it is not a retired capability. Later tags add updater, cancellation,
logging, music, GUI and compression compatibility without another platform
route.

Every donor config key was classified. Real answer delay, reported answer time
and Skip duration match runtime schema revision 2. Historical
`class_task/myself_task`, manual task type/name choice and batch iteration are
product scheduling over complete TaskInventory plus durable per-Task execution.
`br_choices/accept_encoding` is HTTP transport compatibility. Token is secret
material; version/known-version/read/music fields are UI. Issues 51, 64 and
68/69 confirm cumulative-duration mutation, batch-all UX and additional
self-study selection were requests explicitly not implemented by the donor.

The complete question-mode list remains 0, 11, 13, 15-18, 21-22, 31-32,
41-44 and 51-54, all mapped by Asterism. Issue 99 remains only structural
evidence for mode 73 and the maintainer explicitly reports no direct encoding;
the existing explicit Skip path is the full evidenced ability. The issue sweep
found no other sanitized Question or mutation wire shape. Release-only device
blocking/telemetry, dormant uncalled translation, updates, logs and UI are
non-platform behavior.

The shared corpus/policy addendum was checked across the same full evidence
set. No route enumerates attempt history, submitted answers, per-Question
correctness, a passing threshold, retake count/eligibility or reset. The inline
`chance_num`/`answer_state` values describe only the current Question, and
VerifyAnswer returns only a rotated topic token. First-bind history bootstrap
therefore has no Cidaren answer evidence to harvest; normal SubmissionVerify
continues to verify only task-level 100%/Completed plus optional score. Strict
Completion is supported, but Score Improvement has no safe retake operation.
Main must keep those states independent and represent Cidaren retake/evidence
availability as unknown/unsupported rather than deriving it from score.

No migration omission and no new Cidaren-owned route/parser gap was found. No
code or fabricated fixture was added merely to make the sweep look productive.
Current sanitized fixtures already cover every reusable observed shape, and
the full Provider suite plus strict Clippy remain the executable regression
gate. The authenticated-browser, live-account, authorized-mutation and mode-73
evidence blockers remain exact. This completes Cidaren's one-time full sweep;
future checkpoints resume ordinary branch/release/issue delta review.

### 2026-08-15 complete-answer-coverage boundary

The three recorded donor refs and tag sets were rechecked before and after this
incremental checkpoint. They remain `ularch/master@bce9559`, public latest
tag/release `1.5.4@7e29ee4`, `MOPELotus/master@a74b4a2` and historical
`main@1409858`; the public issue update ordering remains headed by the already
audited 112/113 cluster. No new commit, tag, release, issue shape, route or
setting entered the capability audit.

Core's immutable Draft can now represent a complete answered/unanswered
snapshot partition for Providers with evidenced partial submission. The full
Cidaren donor sweep found no such setting or behavior: each assessment step
has one current Question and requires an explicit Answer or explicit Skip.
Cidaren now enforces that evidence boundary after generic Draft validation and
before any execution/verification I/O: coverage must be exactly 1000/1000,
unanswered IDs must be empty and total count must equal the Draft item count.
A generic Domain-valid 50% Draft is fixture-covered and fails closed. This
adds no Cidaren partial policy or runtime setting.

### 2026-08-15 verified completion-diagnosis boundary

The donor refs, latest public tag/release and issue update head were rechecked
at this checkpoint and did not move. Cidaren registers no TaskExecution slot,
so only its fresh SubmissionVerify snapshot can provide a Provider-specific
completion diagnosis.

The existing verified facts support one exact mapping. A class Task with
audited `over_status == 3` is explicitly expired, so its incomplete snapshot
maps to `WindowClosed`. Completed snapshots need no diagnosis. Task-level
`NotOpen/Pending/InProgress` plus 0-99% proves an unfinished Task but does not
identify pending children or another cause. Those states, unknown/inconsistent
state, score, duration, prerequisite, review and attempt-limit cases remain
unmapped because the donor exposes no reliable corresponding fact. Provider
tests freeze the expiry mapping and conservative boundaries.

### 2026-08-15 post-Strict-Completion continuation audit

The recorded donor branches remain `ularch/master@bce9559`,
`MOPELotus/master@a74b4a2` and historical `main@1409858`; public latest
tag/release remains `1.5.4@7e29ee4`, and the issue update head remains the
already audited 112/113 cluster. No new donor route, setting, result field or
Question wire entered scope.

Core revisions through `1824fb4` persist per-attempt completion observations
and continue only an active workflow whose verified diagnosis permits another
attempt. Re-reading every Cidaren slot found no new Provider-only implementation
unit: Cidaren has no TaskExecution capability, history/retake route or
retry-eligible verified incomplete reason. Its explicit `WindowClosed` and
fallback `RemoteUnknown` both stop automatic continuation. Mapping ordinary
0-99% progress to a retry-eligible reason would fabricate protocol authority.

The task-family classification was also rechecked. Donor `task_type == 2`
explicitly identifies a class test and remains `SourceType::Exam`, but no
independent row/result fact establishes Formal assessment nature; the existing
Routine classification therefore remains unchanged. The four blockers in
`CAPABILITY_MAP.md` are still exhaustive: authenticated browser context, live
account validation, authorized live mutations and mode-73 answer encoding.
This checkpoint adds no speculative Provider code or fixture.

### 2026-08-15 sanitized protocol-observation checkpoint

Immediately before this checkpoint, the recorded refs were re-read as
`ularch/master@bce9559f536ebbdad791f41ed4e111b30accb05d`, public tag
`1.5.4@7e29ee43692f4c0807fae8cf7f74a5a674793097`,
`MOPELotus/master@a74b4a2c1cdc5d38f568d41ccb11bb4d441ee4f1` and historical
`main@1409858800f3c4bd27577a08049bf1f8d17a069c`. None moved, so no donor
capability or protocol delta entered scope.

Core `63a71a6` introduced validated, bounded Provider protocol observations.
Cidaren now attaches them to three already fail-closed branches: unknown
numeric Question mode, unknown class-page task type and unknown response
encoding version. The first two retain only their integer discriminator; the
last retains only ASCII classification and byte length. Raw responses,
identities, Question/answer content and authentication material remain outside
the observation boundary. Ordinary-study task-type drift, unknown
`over_status` and generic malformed-field/result shapes remain unobserved for
future bounded work rather than being inferred from error messages.

### 2026-08-15 task-vocabulary observation follow-up

The three recorded donor default branches and both tag sets were rechecked as
`ularch/master@bce9559f536ebbdad791f41ed4e111b30accb05d`,
`MOPELotus/master@a74b4a2c1cdc5d38f568d41ccb11bb4d441ee4f1` and historical
`main@1409858800f3c4bd27577a08049bf1f8d17a069c`. Public release head remains
`1.5.4@7e29ee43692f4c0807fae8cf7f74a5a674793097`, the issue update ordering is
still headed by the audited 112/113 cluster, and the imported OAuth handoff
document hashes still match its manifest. No protocol or capability delta
entered scope.

Cidaren now completes the bounded Task-vocabulary observations deferred by
the preceding checkpoint. An ordinary `StudyTask/List` row whose `task_type`
is not the evidenced integer 3 fails the complete inventory with
`TaskInventory / UnknownTaskType`; a class row whose `over_status` is outside
1/2/3 fails with `TaskInventory / FieldDrift`. Each observation retains only
the parsed integer. It never includes the surrounding row, Course/Task
identity, title, score, progress, timing, response body or authentication
material, and it does not normalize an unknown value into an executable Task.

### 2026-08-15 assessment-envelope observation follow-up

The recorded donor refs, tag sets, public `1.5.4` release head, public issue
ordering and imported OAuth handoff manifest remained unchanged after the Task
vocabulary unit. No new result code, envelope shape, route or mutation entered
the capability map.

The shared assessment-step parser and separate word-selection receipt parser
now attach one bounded `Other / UnknownResultShape` observation to malformed
or unrecognized JSON envelopes. The observation records only the fixed parser
family, root/code/message/data/jv value kinds, an optional numeric code and
data truthiness. It omits every localized message and `data`/`jv` value,
including answers and rotating topic codes. The original `ProtocolDrift` or
`InvalidResponse` remains the returned error, and the issued mutation remains
non-replayable under the existing QuestionSession operation ledger.

### 2026-08-15 task-score observation and atomic-verification audit

The three donor refs, tag/release heads, public issue ordering and OAuth
handoff manifest remained unchanged. Main's `ee62903` added an independent
read-only verification record to the TaskExecution atomic-mutation sink. It
does not create a Cidaren integration point: Cidaren uses the separate
QuestionSession/SubmissionExecute step ledger, and no donor route provides
per-ordinal readback for its five assessment mutations. A rotated topic code
or accepted receipt therefore cannot be recorded as verified, and Cidaren
does not register TaskExecution merely to consume the new sink.

The evidenced readback remains terminal whole-Task SubmissionVerify. Its
fresh task-score parser now attaches bounded `SubmissionVerify /
UnknownResultShape` observations to malformed/non-success envelope shapes and
decoded `score/task_score/grade` drift. Envelope observations retain only JSON
value kinds and an optional numeric result code; decoded observations retain
only the three alias kinds and non-null alias count. Score values, unrelated
fields, answers, topic codes, `jv` content and the response body are omitted.
This does not make score completion evidence or weaken the separate fresh
100%/Completed requirement.

### 2026-08-15 inventory-envelope observation follow-up

The recorded donor refs, tags/releases, public issue update head and OAuth
handoff manifest were rechecked after the pushed checkpoint and did not move.
No inventory route, result code or field family entered scope.

Cidaren's class-page and selected-Course study-list parsers now attach bounded
`TaskInventory / UnknownResultShape` observations when their outer result
envelopes drift. The class shape retains root/code/data/records/total kinds,
an optional numeric code and bounded record/total counts. The study shape
retains root/code/data/Course/list field kinds, an optional numeric code and
bounded list count. Neither shape contains a row, Course/Task identity, title,
score/progress/time value, answer material or credential. Existing
`ProtocolDrift`/`InvalidResponse`, complete-pagination rejection and fresh
identity rebinding remain unchanged, so terminal verification cannot continue
from a partial or newly guessed inventory.

### 2026-08-15 account-validation observation follow-up

The donor refs, public/historical tags and releases, public issue update head
and OAuth handoff manifest remained unchanged after the inventory-envelope
unit. No account route, result code or profile field family entered scope.

The shared `Student/Main` classifier now distinguishes expected credential
rejection from protocol drift at the observation boundary. A well-formed
non-success code remains ordinary `Authentication` with no observation.
Malformed root/code shape, `code=1` without a non-empty `data.user_info`
object, and invalid selected-Course shape attach only account-envelope value
kinds, numeric success code and profile-field count as `Authentication /
UnknownResultShape` or `FieldDrift`. The zeroizing response owner still clears
student name/code, school, class and Course ID values; none enters the shape.
ImportedToken, captured token/Composite and native OAuth Composite validation
all continue through this same fresh account readback before use or commit.

### 2026-08-15 answer-evidence observation follow-up

The donor refs, public/historical tags and releases and public issue update head
remained unchanged after the account-validation unit. No new evidence route,
answer family or direct mode-73 encoding entered scope.

The four implemented answer-evidence reads now attach a bounded
`AnswerResolve / UnknownResultShape` observation when a valid JSON envelope or
decoded inventory/word shape fails closed. The shape retains only the fixed
route family, root/code/data/version value kinds, optional numeric code,
object/array counts and known word/list/evidence field-kind counts. Word,
meaning, example, Course ID, list ID and raw response values remain owned by the
zeroizing document and do not cross the observation boundary. Fresh binding
changes remain `RemoteChanged` with no protocol observation, and existing
unknown-`jv` observations are preserved rather than overwritten.

### 2026-08-15 Question-payload observation follow-up

The donor refs, public/historical tags and releases and public issue update head
remained unchanged after the answer-evidence unit. No new topic mode or mode-73
direct-answer encoding entered scope.

Known-mode Question and reading-card payload failures now attach a bounded
`QuestionParse / UnknownResultShape` observation after caller Task/position
binding succeeds. It retains only root and known field kinds, object/array and
field-kind counts, and the numeric topic mode. Rotating topic code, stem,
option, answer-tag and relation values remain excluded. The existing exact
`UnknownQuestionKind` observation for an unaudited numeric mode is preserved,
while invalid caller binding remains an unobserved local protocol error. This
does not weaken mode-73's explicit Skip-only execution evidence boundary.

### 2026-08-15 OAuth V2 response-observation follow-up

The donor refs, public/historical tags and releases, public issue update head
and OAuth V2 handoff manifest remained unchanged after the Question-payload
unit. No login route, version or cryptographic transcript entered scope.

Malformed successful native OAuth V2 envelopes and authenticated
handshake/payload failures now attach a bounded `Authentication /
UnknownResultShape` observation. It retains only root/known field kinds and
counts, numeric success code, handshake/encryption booleans, fixed-AAD match and
encoded string lengths. OAuth callback code, server public key, salt, IV,
ciphertext, response version and decrypted token values remain in zeroizing
owners and do not cross the observation boundary. A well-formed non-success
code and a successfully decrypted but invalid token remain unobserved ordinary
Authentication; the one-shot no-replay exchange contract is unchanged.

### 2026-08-15 response-data framing observation follow-up

The donor refs, public/historical tags and releases, public issue update head
and OAuth V2 handoff manifest remained unchanged after the login-handshake
unit. No response transform, inserted-byte index, chunk order or authenticated
payload format entered scope.

Every exact known response-decoder family now attaches a bounded `Other /
UnknownResultShape` observation when data framing, base64, confusion-byte,
chunk reconstruction or authenticated AES-GCM decoding fails closed. It
retains only the fixed decoder family, data/payload kinds and counts,
ASCII/whitespace facts and encoded field lengths. Encoded data, AAD and
ciphertext values do not cross the observation boundary. Unknown `jv` values
continue to use the smaller value-free `EndpointVersionDrift` shape, while a
missing `jv=99` crypto context remains unobserved Authentication. No decoder
fallback, candidate guessing or mutation retry was added.

### 2026-08-15 Native HTTP response-head observation follow-up

The donor refs, public/historical tags and releases, public issue update head
and OAuth V2 handoff manifest remained unchanged after the response-framing
unit. No route, redirect behavior or content type entered scope.

All fixed Native HTTP routes now attach a bounded `Other /
UnknownResultShape` observation for redirect/404 and other unexpected
non-success statuses, or missing/invalid/non-JSON Content-Type. Status shapes
retain only fixed route and numeric status; content-type shapes retain only
header presence/UTF-8, media-type ASCII/length, parameter count and JSON-suffix
fact. URL, raw header, response body and credential values remain excluded.
401/403 Authentication, 429 with bounded Retry-After and 5xx
ProviderUnavailable remain unobserved operational errors, and redirects remain
disabled rather than followed.

### 2026-08-15 Native HTTP response-body observation follow-up

The donor refs, public/historical tags and releases, public issue update head
and OAuth V2 handoff manifest remained unchanged after the response-head unit.
No response body format or route maximum entered scope.

After a successful response head, declared-length overflow, streamed overflow,
empty body and invalid UTF-8 now attach a bounded `Other /
UnknownResultShape` observation. It retains only fixed route/state, observed
byte length and the route's configured maximum. Response bytes do not cross the
observation boundary, and accumulated buffers are zeroized before every
rejected return. Chunk-read transport failures keep their existing unobserved
Network/InvalidResponse classification; no body replay or relaxed bound was
added.

### 2026-08-15 Task-row shape observation follow-up

The donor refs, public/historical tags and releases, public issue update head
and OAuth V2 handoff manifest remained unchanged after the response-body unit.
No Task row field or vocabulary value entered scope.

After a valid complete class/study envelope, malformed Task rows now attach a
bounded `TaskInventory / FieldDrift` observation. It retains only row kind,
object field count and fixed known field kinds. Task/Course/list identities,
titles, score, progress and time values remain excluded. Existing exact
unknown-task-type and unknown-class-status observations are preserved rather
than replaced. Every row failure still rejects the complete inventory before
any partial or stale Task can reach TaskDetail, progress or terminal readback.

### 2026-08-15 Native JSON syntax boundary follow-up

The donor refs, public/historical tags and releases, public issue update head
and OAuth V2 handoff manifest remained unchanged after the Task-row unit. No
response body syntax or route parser shape entered scope.

The shared Native HTTP reader now validates one complete JSON value with a
non-retaining `IgnoredAny` deserializer before returning the owned UTF-8 string
to any route parser. Invalid syntax and trailing data zeroize the string and
attach the existing bounded response-body shape with only fixed route,
`invalid_json`, observed length and configured maximum. Response text and a
duplicate parsed tree do not cross the boundary. Route parsers remain
responsible for semantic shape and protocol-vocabulary checks.

### 2026-08-15 current-Question state projection follow-up

The donor refs, public/historical tags and releases and public issue update head
remained unchanged before this unit. No new Question mode, result route or
mode-73 direct-answer encoding entered scope.

The paired integer `chance_num`/`answer_state` shape from public issue 99 is now
parsed as a bounded raw current-Question observation and persisted only under
`cidaren_current_question_state`. Both fields must be present together and lie
within `0..=100000`; partial, fractional, negative or oversized shapes fail
closed and attach only their JSON value kinds to the existing Question payload
observation. State-only changes do not alter the stable remote Question
identity. The values remain neither correctness receipts nor history/retake
authority, and this adds no direct mode-73 answer encoding or mutation retry.

### 2026-08-15 real-Question progress recovery follow-up

The donor refs, public/historical tags and releases, public issue update head
and OAuth V2 handoff manifest remained unchanged before and after this unit. No
new progress field, mutation route or current-attempt readback entered scope.

The already audited optional `topic_done_num/topic_total` observation now
round-trips through the encrypted real-Question continuation as paired
`progress_completed/progress_total` fields. Decode reuses the parser's bounded
`completed <= total <= 100000` invariant, rejects incomplete or contradictory
pairs, and treats their absence in an older v2 artifact as `None`. Initial and
rotated Question recovery expose the same remote observation again without
using it as local position, Question identity, answer selection or mutation
routing. Reading-card recovery retains its existing equivalent boundary.

### 2026-08-15 typed current-Question state recovery follow-up

The donor refs, public/historical tags and releases, public issue update head
and OAuth V2 handoff manifest remained unchanged before and after this unit. No
result, retake or direct mode-73 answer route entered scope.

The already bounded public-issue-99 `chance_num`/`answer_state` pair now has
a read-only `CidarenCurrentQuestionState` boundary. Fresh parsing and
fingerprint-bound Question recovery both construct the same type, and
`CidarenAttemptFlow::current_question_state` exposes it before and after
encrypted continuation recovery. Restored metadata must contain exactly the two
bounded integer fields; malformed or extended objects fail closed. The remote
progress and state-code helpers also use their own named bounds rather than
cross-referencing equal-valued constants. Neither code is interpreted as
correctness, history, Task-retake authority or mode-73 answer encoding.

### 2026-08-15 attempt-response identity follow-up

The donor refs, public/historical tags and releases, public issue update head
and OAuth V2 handoff manifest remained unchanged before and after this unit. Re-reading
the public issue-99 diagnostic confirmed a paired integer `task_id/task_type`
echo on the decoded current-Question payload; no stable response release/list
identity or new route was observed.

`CidarenAttemptResponseIdentity` now parses the pair only when both fields are
present. Task ID must be `-1` or positive and Task type must be the audited row
family 1/2/3. Before accepting a Question or reading card, the flow checks the
echoed row type against its fresh assessment binding and requires an already-
positive fresh Task ID to match. A fresh `-1` may receive a positive dynamic
allocation, but it is not persisted or substituted for class release or study
course/list identity. Malformed/partial shapes attach only value kinds;
mismatches are `RemoteChanged` and leave the issued non-idempotent operation
FailedClosed without replay.

### 2026-08-15 dynamic attempt-allocation and prerequisite re-audit

The donor refs/tags/releases remain unchanged. A value-free scan of 30 public
diagnostic attachments found multi-step decoded Question sequences in issues
70, 77 and 85. Issue 77 has one Start, 21 Next operations, 22 distinct topic
tokens and exactly one response Task ID/type; the other two independently keep
one allocation across 9 and 34 Question rows. Raw IDs, tokens, Questions,
accounts and logs were not retained or copied into fixtures.

The first positive response allocation after a fresh `task_id=-1` is now bound
to the attempt, but never promoted to stable Task identity. Both encrypted v2
continuation families carry the optional positive ID and accept older v2 state
without it. Recovery reconciles it with a newly positive fresh row; later
Question/reading-card responses must match. Invalid or switched allocations
fail closed as `RemoteChanged` without replay.

The same re-audit confirmed a separate Core Gap. Issues 48/49 repeatedly show
`SubmitChoseWord` definitely rejected with `code=0`, null data and the exact
incomplete-section prerequisite message. `ProviderQuestionReadStepOutcome`
cannot persist a blocked `RequiredChildrenPending` result with response digest
and observation time. Cidaren therefore keeps the response fail-closed and
non-replayable; Main owns the required shared blocked-step extension.

### 2026-08-16 class Start-selector identity follow-up

The recorded refs were re-resolved as
`ularch/master@bce9559f536ebbdad791f41ed4e111b30accb05d`,
`MOPELotus/master@a74b4a2c1cdc5d38f568d41ccb11bb4d441ee4f1` and
historical `main@1409858800f3c4bd27577a08049bf1f8d17a069c`. The public and
owner tag sets still end at `1.5.4@7e29ee43692f4c0807fae8cf7f74a5a674793097`;
the historical tags still end at `v3.73`, and the release lists have no newer
published entry. No donor protocol or capability delta entered scope.

Re-reading the reopened donor's `main_window.py` and `main_api.py` clarified an
existing asymmetric identity rule. The UI fixes `PublicInfo.task_type_int=2`
for class execution even when the selected inventory row is class learning,
and `StartAnswer` sends that value with `release_id`. Decoded payloads instead
echo the actual row family, including type `1` for class learning. The Rust
binding already retained separate request-family and response-row fields; this
checkpoint names the request field explicitly and adds a regression proving a
class-learning Start request sends `2` while only response type `1` satisfies
the fresh row identity. Dynamic allocation, stable release identity and
non-replay semantics are unchanged.

The same identity audit also closed the remaining direct recovery-test gap for
mode-0 reading cards. A synthetic Start payload now binds one positive dynamic
allocation into `cidaren.pre-question-attempt.v2`; recovery asserts that exact
value and a subsequent reading-card payload with another allocation leaves
`SubmitAnswerAndSave` FailedClosed as `RemoteChanged`. This adds no new wire
claim and preserves the same no-replay boundary already used by real Questions.

### 2026-08-16 definite prerequisite-rejection boundary

The three default refs and recorded latest tags were re-resolved unchanged as
`ularch/master@bce9559f536ebbdad791f41ed4e111b30accb05d`,
`MOPELotus/master@a74b4a2c1cdc5d38f568d41ccb11bb4d441ee4f1`, historical
`main@1409858800f3c4bd27577a08049bf1f8d17a069c`, public/owner `1.5.4` at
`7e29ee43692f4c0807fae8cf7f74a5a674793097` and historical `v3.73` at
`152278daf3a5822b11eb2a3064924bf50d3c18b6`. No new donor capability or
protocol delta entered scope.

Public issues 48/49 were re-read at the response boundary. They repeatedly
show the same response-only five-field `SubmitChoseWord` envelope: numeric
zero code, the incomplete-section prerequisite message, null data and string
`jv=cv=0`. Cidaren now classifies only that exact shape as
`CidarenAssessmentRejectionKind::RequiredChildrenPending`; any changed message,
truthy data, version or extra field remains ordinary fail-closed result-shape
drift. The Native transport can therefore retain the raw response digest and
actual receive time, and the attempt flow exposes a non-replayable
`CidarenDefiniteRejection` before returning stable non-retryable provider code
`cidaren.required-children-pending.v1` through the current public adapter.

This narrows but does not cross the shared gap. `ProviderQuestionReadStepOutcome`
still cannot persist a blocked diagnosis with that digest/time, so no durable
`RequiredChildrenPending` step is fabricated. The Provider stays FailedClosed
and a second `SubmitChoseWord` remains impossible.

### 2026-08-16 blocked-step recovery artifact follow-up

No donor ref, tag or response evidence changed after the definite-rejection
unit. Cidaren now carries the exact frozen request digest into
`CidarenIssuedOutcome` and `CidarenDefiniteRejection`, then may encode the
rejection as `cidaren.question-blocked-step.v1`. The bounded zeroizing artifact
binds local Task ID, stable remote Task ID, exact SubmitChoseWord operation,
RequiredChildrenPending kind, request digest, response digest and canonical
nanosecond UTC receive time. Recovery requires the artifact digest plus exact
Task and request binding; foreign Task, operation/reason drift, unknown fields,
zero/changed digests or non-canonical time fail closed.

The current public Question adapter intentionally still returns the stable
non-retryable provider error and does not persist the artifact. Main owns the
future durable blocked-step record, encryption/storage and Engine transition;
this Provider unit supplies the complete no-guess recovery payload without
claiming that shared authority.

### 2026-08-16 blocked-step flow recovery follow-up

The first artifact unit retained the immutable request/response evidence but
did not yet distinguish an initial selection block from a later mid-attempt
selection block, or bind the positive dynamic Task allocation already carried
by the attempt. `CidarenDefiniteRejection` and
`cidaren.question-blocked-step.v1` now also retain the bounded current position
and optional positive `remote_attempt_task_id`.

After the caller decrypts and digest-validates the artifact,
`CidarenAttemptFlow::restore_definite_rejection` repeats fresh context and
stable Task rebinding, requires the dynamic allocation to remain exact and
restores only `FailedClosed(SubmitChoseWord)` with the same definite evidence.
It cannot recreate a word map, Ready state or mutation command. A changed fresh
Task ID fails `RemoteChanged`; successful recovery re-encodes the identical
artifact digest. Main still owns encrypted persistence and the shared blocked
outcome, but no additional Cidaren execution cursor is now missing from that
future record.

### 2026-08-16 blocked-step atomic handoff follow-up

The recovery artifact and flow state were complete, but a future caller could
still read the rejection and encoded artifact as separate Provider values and
accidentally pair them with another diagnosis or ledger tuple.
`CidarenBlockedStepMaterialization` now consumes the exact artifact and
definite rejection into one immutable handoff containing
`RequiredChildrenPending`, `SubmitChoseWord`, request digest, response digest
and receive time. Its getters and consuming `into_parts` expose the same frozen
tuple, and Debug keeps only the already-redacted artifact plus typed facts.

The current shared adapter still does not return this handoff. Main owns the
new shared outcome and persistence, while Cidaren now supplies one atomic value
rather than requiring Core to reconstruct Provider semantics from parallel
fields.

### 2026-08-16 blocked-step atomic binding follow-up

The first atomic handoff accepted an already encoded artifact next to the
definite rejection. Although the normal flow created both from one value, that
constructor shape did not itself prove they represented the same position,
dynamic allocation, request/response digests and receive time.

The materialization constructor now accepts the unencoded Provider artifact,
compares every shared field with the definite rejection and only then performs
canonical encoding. A deliberately changed response digest is covered as
`ProtocolDrift`; operation, reason, position, allocation, request digest and
receive-time drift use the same fail-closed boundary. This is Provider-owned
binding work only; Main still owns encrypted persistence and the shared blocked
Question-step outcome.

### 2026-08-16 grouped word-selection wire follow-up

The incremental checkpoint again resolved `ularch/HEAD` and `master` to
`bce9559f536ebbdad791f41ed4e111b30accb05d`, `MOPELotus/HEAD` and `master` to
`a74b4a2c1cdc5d38f568d41ccb11bb4d441ee4f1`, and the public latest tag/release
to `1.5.4@7e29ee43692f4c0807fae8cf7f74a5a674793097`. No branch, tag or release
delta was present. The public issue ordering also remained headed by issues
112/113; neither adds a learning-platform route or response shape.

Re-reading both pinned `select_all_word` call families exposed a pre-existing
migration omission: ordinary units explicitly wrap their fresh words as
`{course_id:list_id: [words...]}`, while the self-built path first groups the
fresh inventory by the same key and then passes that object. No eligible donor
call site supplies a bare word array. `build_word_selection_plan` now always
emits the grouped map, preserving one group for an ordinary unit and multiple
groups when a self-built inventory spans units. The signed mutation builder
remains unchanged and continues bounding, compact-encoding and zeroizing that
exact map.

### 2026-08-16 existing-selection continuation correction

The same source re-read found that the earlier blocked-step interpretation was
incomplete. In `ularch@c8de5f4`, the ordinary-learning caller catches a failed
`SubmitChoseWord`, records that the task is already started and proceeds to
`StartAnswer`. Issue 49's maintainer comment supplies the missing semantic
explanation: the affected learning Task already had word selection, while the
old automation assumed it had not and tried to select every word again.
The historical `github123666/cidaren@1409858` was re-read to bound the route:
it explicitly executes direct `StudyTask` assessment operations, but its
`complete_practice` path has no continue-on-selection-error behavior. The
current catch is therefore evidence for ordinary class learning only, not a
generic response rule for every mutation family.

This entry supersedes the blocked-step/Core-Gap direction recorded immediately
above. Asterism does not copy the donor's broad exception catch. Only the exact
issues 48/49 five-field response is classified as
`ExistingWordSelection` (renamed from the provisional
`RequiredChildrenPending` label), and only an ordinary class-unit plan may
rotate that response into the durable pre-Question artifact phase
`cidaren.ready-to-start` with the accepted response digest and receive time.
Recovery fresh-rebinds the Task and dynamic allocation, cannot rebuild or
resend the word map, and issues `StartAnswer` as the next distinct operation.
The same response on a self-built class or direct `StudyTask` plan, near-match
envelopes and all unrelated failures still fail closed.

Because the exact rejection is now an evidenced pre-Question continuation,
not a Task completion diagnosis, the provisional
`cidaren.question-blocked-step.v1`, definite-rejection materialization and
`CIDAREN-CORE-GAP-QUESTION-BLOCKED` are removed. The existing shared
Question-read continuation already carries the required durable state and
operation-ledger response binding; no shared Core/Storage/Engine change is
needed for this donor behavior.

Post-implementation refs remained `ularch/master@bce9559f536ebbdad791f41ed4e111b30accb05d`,
`MOPELotus/master@a74b4a2c1cdc5d38f568d41ccb11bb4d441ee4f1` and historical
`github123666/main@1409858800f3c4bd27577a08049bf1f8d17a069c`; tag `1.5.4`
still targets `7e29ee43692f4c0807fae8cf7f74a5a674793097`. The scoped Provider
suite retained 190 unit tests plus two doctests, and all-target/all-feature
Clippy remained warning-free.

### 2026-08-16 StartAnswer query-encoding follow-up

All three frozen donor lines pass the literal string `%23000000` as
`opt_font_c` in their `StartAnswer` params. Their standard Python requests
encoder escapes the percent sign, so the raw URL contains
`opt_font_c=%2523000000`; the decoded application-level query value remains
`%23000000`. Asterism previously substituted semantic `#000000`, which emitted
only `%23000000` on the wire and changed the durable request digest.

`build_start_answer_request` now freezes the donor literal for both ClassTask
and StudyTask. The protocol-vector test checks the value before transport, and
the Native HTTP boundary checks the double-encoded raw URL. No credential,
mutation scheduling or response semantics changed.

### 2026-08-16 post-correction stop-rule checkpoint

The final ref check after the grouped selection, existing-selection recovery
and Start query corrections still resolves public
`master@bce9559f536ebbdad791f41ed4e111b30accb05d`, owner
`master@a74b4a2c1cdc5d38f568d41ccb11bb4d441ee4f1`, historical
`main@1409858800f3c4bd27577a08049bf1f8d17a069c` and tag
`1.5.4@7e29ee43692f4c0807fae8cf7f74a5a674793097`. No new branch/tag protocol
delta appeared.

Every audited donor call site is now implemented or covered by currently
executable Provider verification. The only remaining gates are the authorized
authenticated-browser context, real-account/live-mutation validation and new
mode-73 answer-wire evidence already listed in `CAPABILITY_MAP.md`; each needs
external state or new evidence rather than another Cidaren-only code change.
This satisfies the repository Provider stop rule until Main or a future donor
delta reopens the worker.

## Check procedure

For the next checkpoint:

1. resolve each repository's symbolic `HEAD` and full default-branch SHA;
2. list new tags and releases, including asset digest/size metadata when a
   packaged release could carry source-visible behavior;
3. diff the prior snapshot to the new revision by executable call site,
   protocol constant, authentication/Capture path and response transform;
4. update the capability/protocol difference tables before porting code;
5. run Provider tests, all-target clippy and scoped diff checks;
6. re-read both refs immediately before declaring the checkpoint complete.
