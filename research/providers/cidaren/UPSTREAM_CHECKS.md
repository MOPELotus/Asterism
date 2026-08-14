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
