# chaoxing upstream sources

Audit date: 2026-08-16

This is a static source audit. No donor or Asterism implementation was live-tested
against a real account during this audit, so every live-validation claim remains
pending.

| Source | Revision | Updated | Use | Audited implementation surface | Live status |
|---|---|---|---|---|---|
| [`Samueli924/chaoxing`](https://github.com/Samueli924/chaoxing) | `9699e632b492cdc55ea35a1ab05b6dfcbfb7cf70` | 2026-08-14 | Reference | Password/Cookie login, course/folder inventory, Chapter cards, Video/Audio fallback, Document, Live, Read, Chapter Work, sign-in and learning-count behavior | Pending |
| [`surinrasu/CxKitty`](https://github.com/surinrasu/CxKitty) | `1589eac9c07c4bab71f79d762b45210643dd537d` | 2024-09-29 | Reference | Password/QR login, SSO validation, mobile course/Chapter APIs, Video/Document, Work/Exam export/save/submit, retake facts, face/captcha branches | Pending; protocol age is a risk |
| [`Ylim314/chaoxing-sign`](https://github.com/Ylim314/chaoxing-sign) | `7ed64ff547d352708066ff61f1b1dc1fb1be32f1` | 2026-03-09 | PortSource | Current `activelist`/`signDetail` reads, sign activity/status codes, structured time fields, normal/QR/gesture/location/code dispatch, WebIM event monitoring and application-local sign logs | Pending |
| [`superdaobo/mini-hbut`](https://github.com/superdaobo/mini-hbut) | `952640e2540f9daf927ec940a2a2862b5cd4dd34` | 2026-08-16 | Reference | Independent current activity-list/detail routes, numeric-or-string identities/times, variant normalization and sign-state heuristics | Pending; current delta does not change the Chaoxing protocol tree |
| [`iwillwill-ALLWILL/chaoxing-agent-skill`](https://github.com/iwillwill-ALLWILL/chaoxing-agent-skill) | `f72619a0b36996d27d00577015663ec39e782500` | 2026-06-17 | PortSource | Browser Work/Exam inventory, rich editor filling, result inspection, retry/retake policy and current DOM reliability rules | Donor reports real use; Asterism validation pending |
| [`ocsjs/ocsjs`](https://github.com/ocsjs/ocsjs) | `890686a5e54f9a6d52d1169bae9ea5971e0863c7` | 2026-07-01 | Reference | Current media/PPT/Read/Chapter-test/Work/Exam lifecycle, thresholded save/submit, extended Question types and browser controls | Pending |
| [`LangHY/chaoxing-exam`](https://github.com/LangHY/chaoxing-exam) | `14e1dfd9cf11cd54dabb494dd01e318856d9b8d3` | 2026-06-19 | Reference | Chapter-test navigation, fill/short editor behavior, result standards, score and `redoTest` pitfalls | Donor reports Chapter-test coverage; Asterism validation pending |
| [`CodFrm/cxmooc-tools`](https://github.com/CodFrm/cxmooc-tools) | `2b81f7b55a68ea2ceb6ea0312a6791ebf3ed3dc5` | 2021-11-03 | Historical | Older Video/Audio/Document/Read, Work/Exam question and result-collection routes | Not current enough for implementation authority |

## Source selection

- Use the MIT-licensed agent skill as the primary behavior source for independent
  Work/Exam discovery, status classification, and result verification.
- Use the current OCS branch to cross-check Browser routes and DOM behavior, not
  as a task inventory implementation.
- Use `Samueli924/chaoxing` for the existing Chapter path and account/session behavior.
- Use CxKitty to understand the complete mobile API call chain and typed failure
  branches, but do not assume its 2024 transport still works in 2026.
- Use `chaoxing-sign` as the primary current source for the read-only activity
  list/detail shape. Use `mini-hbut` only as independent corroboration and keep
  its GPL implementation reference-only.
- The two current sign-in donors conflict on `status=1` and `otherId=5`.
  Asterism therefore retains raw status codes and models `otherId=5` as
  ambiguous instead of inferring account completion, eligibility or a mutation
  variant.
- Ylim's WebIM path is event discovery, not completion verification. Its
  `sign_history` endpoint reads the donor application's own Prisma `SignLog`
  rows written after local execution and contains no Chaoxing remote readback.
- Use `chaoxing-exam` only as a Browser/DOM reference for non-formal Chapter tests.
- Use `cxmooc-tools` only to diagnose historical route variants.

No donor source is vendored into Asterism by this audit.

## AnswerResolution audit

The pinned donors expose four distinct answer sources which must not be merged:

- `Samueli924/chaoxing` calls a user-configured `Tiku.query_all`; absent or
  unmatched results fall back to random answers, and its README explicitly says
  correctness is not guaranteed.
- CxKitty requires configured REST, JSON, SQLite or OpenAI searchers and can use
  a random/fuzzer fallback. These are external-bank/model inputs, not Chaoxing
  Provider HTTP evidence.
- OCS requires non-empty answer-wrapper configuration or its browser-local cache.
  The cache is populated from prior wrapper/results and is not a platform
  standard-answer endpoint.
- The agent skill consumes pre-computed answers and focuses on Browser filling,
  persistence and verification. It does not define an answer source.

The only reproducible Provider-native standard-answer evidence is the pinned
`chaoxing-exam` Chapter result DOM: a completed
`selectWorkQuestionYiPiYue` iframe exposes `正确答案` beside exact Questions.
Asterism therefore implements a typed but unregistered Chapter-result
`AnswerResolveCapability` which performs fresh TaskDetail rebinding, delegates
one fully bound read to an abstract result transport, and then applies the
existing strict QID/order/type/current-option parser. Pending Work/Exam Questions
have no audited Chaoxing standard-answer protocol, so they remain a hard blocker
rather than receiving external or guessed answers. Registration additionally
waits for the BrowserBridge result transport and a Core lifecycle which can
distinguish post-result evidence from pre-submission resolution.

The same audit confirmed CxKitty's separate QR session sequence: read `uuid` and
`enc` from the Web login page under one Cookie jar, activate `createqr`, display
the `toauthlogin` URL, and poll `getauthstatus`. Asterism implements this as a
bounded Native transport behind Core's durable interactive-auth workflow. Its
canonical secret continuation binds the Core account/AuthSession/correlation,
every claim performs one phase-specific remote read, and the Cookie jar enforces
Chaoxing domain/path scope and zeroization. A reported success first persists
the Cookie in an identity-validation continuation; its next claim performs only
a fresh Course-list read before a terminal continuation can finalize the Cookie
bundle. This prevents a transient validation failure from repeating
`getauthstatus`. Metadata now advertises `QrCode` at Development; sanitized live
validation is still required.

## Refresh log

- 2026-08-16: re-checked CxKitty's fixed Exam cover parser at `1589eac9c07c`.
  Its `var *needcode *= *(\d+);` extraction converts any nonzero decimal to
  true. Asterism replaced the prior digit-`1` substring heuristic with one exact
  required declaration: 0 is false, bounded nonzero is true, and
  missing/duplicate/quoted/fractional/lookalike declarations are ProtocolDrift.
  The same cover re-read confirms required `faceRecognitionCompare` and
  `captchaCheck` inputs. Asterism now requires one raw `0|1` value for each;
  missing, duplicate, padded or unknown values are drift rather than false.
  The same cover/start audit now binds `testUserRelationId` without trimming or
  first-match selection: cover requires one exact value, while start may omit
  it but may not supply a padded, foreign or duplicate replacement.
  Start continuation parsing now also requires unique agreeing URL/form `enc`
  and unique untrimmed decimal timing inputs before artifact creation.
  The donor revision is unchanged and no challenge-solving path was added.
- 2026-08-16: re-read CxKitty's session initialization and Exam submission at
  `1589eac9c07c`. The donor installs
  `X-Requested-With: com.chaoxing.mobile` on the session used by
  `reVersionSubmitTestNew`. Asterism's request digest already froze that exact
  mobile header, but the Native final dispatch had substituted the independent
  Work/browser value `XMLHttpRequest`. The transport now builds and tests the
  actual request with the same mobile header as its persisted digest.
  The same session actor supplies the Exam cover `userId` before the resulting
  `testUserRelationId` is sent as `examAnswerId`. Asterism's encrypted
  pre-Question command is now v2, includes that zeroizing actor in its digest
  and rejects an execution-time `_uid|UID` change before the one-shot start.
  CxKitty also creates one process/session `secrets.token_hex(16)` IMEI, embeds
  it in the mobile User-Agent and sends the same value through Exam
  start/preview/submit. The current Asterism placeholder and independently
  configurable Core User-Agent cannot satisfy that coupling; a persisted
  account/device profile is recorded as a Main-owned live-validation gap.
- 2026-08-16: re-read Samueli's web `addStudentWorkNew` POST at
  `9699e632b492`. The exact donor request uses the fresh editor route as Referer
  and `X-Requested-With: XMLHttpRequest`. Asterism had accidentally reused the
  mobile Exam request-family constant for independent and Chapter Work final
  POSTs. Work now has a separate web-AJAX constant and a built-request regression
  covering that header, the retained Referer, the same-origin `Origin` and the
  donor's `application/x-www-form-urlencoded; charset=UTF-8` media type.
  Samueli's submitted body also derives `userId` from the current session actor.
  Native Work/Chapter Work now requires one unique `_uid|UID` Cookie, rejects a
  conflicting fresh-editor `userId`, and fills an omitted field only from that
  same resolved session before the one-shot POST.
  The same donor chain constructs its attempt URL from Chapter `knowledgeid`,
  account-scoped `cpi`, target `workId`, and the card's
  `jobid/originJobId`; the final form is now required to repeat those exact
  fresh target identities before dispatch.
  Its web chain also fixes `api=1, mooc2=1`; Asterism now rejects a missing or
  changed mode flag rather than splicing CxKitty's separate mobile
  `api=1, mooc=0` body into this endpoint family.
  Samueli posts the decoded form to the exact same web
  `/mooc-ans/work/addStudentWorkNew` endpoint. The form parser now requires that
  explicit relative or absolute same-origin action instead of trusting only a
  familiar form ID and silently substituting the static endpoint.
- 2026-08-16: re-checked the pinned agent skill's result-page guidance at
  `f72619a0b369`; it treats `我的答案` as visible per-Question result evidence
  and explicitly audits empty answer rows, not arbitrary substring-derived
  values. Asterism tightened independent Work verification accordingly:
  choices now allow only uppercase option IDs plus explicit separators and
  reject duplicates/single-choice multi-values, while true/false accepts only
  complete controlled tokens. Work and Exam selected result nodes now also
  require exactly one `我的答案` label, so a later conflicting value cannot be
  truncated away. Controlled `未作答`/`待批阅`/`暂无答案`-family Work values are
  retained as missing evidence, producing Inconclusive/Unverified rather than
  ProtocolDrift or rejection. No donor revision changed and no mutation path
  was added. Chapter and Exam result identity attributes are now also checked
  without trimming, so padded QIDs cannot be normalized into the current Draft.
- 2026-08-16: re-checked the two current sign-in donor default branches after
  the QR checkpoint; Ylim remains `7ed64ff547d3` and `mini-hbut` remains
  `952640e2540f`. An exact Ylim `preSign()` re-read recovered an omitted
  auxiliary path: the implementation issues `analysis`, extracts one dynamic
  `code='+'...'` value, then issues `analysis2` before variant dispatch, while
  its own comment labels the pair location-required. Asterism now freezes the
  exact first query and response-bound second query as a separate redacted,
  transport-free continuation. It does not claim ordinary-sign necessity,
  classify the opaque `analysis2` result, or weaken durable issue/readback
  requirements. The same exact comparison against Samueli `9699e632` and
  `mini-hbut` `952640e` corrected the ordinary preparation family: Ylim omits
  Samueli/mini-only `tid`/`ut`, while mini follows its broader pre-sign request
  with a different `/pptSign` route. Asterism now explicitly freezes only
  `YlimPreSignStuSignAjax`; no hybrid donor chain remains.
  The same strict source comparison corrected receipt parsing to byte-exact
  equality: surrounding whitespace around `success`, the already-signed text
  or `success2` now fails as protocol drift instead of being trimmed.
- 2026-08-16: refreshed all eight recorded donor default branches. Seven heads
  remain unchanged. `mini-hbut` advanced from `64fb2f06e10c` to
  `952640e2540f`; its three-commit delta changes schedule cleanup, a separate
  OIDC identity platform, root/website documentation and website behavior. A
  complete recursive-tree comparison shows every Chaoxing check-in module,
  Chaoxing protocol document and related UI blob unchanged, so the current pin
  advances without a Chaoxing capability or wire delta. Re-reading Samueli's
  fixed Work form confirms type 2 still uses one `answer{qid}` plus
  `answertype{qid}` field. Asterism now fixture-covers only the exact
  page-bound single-blank independent Work subset and keeps multi-blank and
  short-answer semantics fail-closed. The same refresh also re-read Ylim's
  unwired QR submit fields and refresh `SIGNIN:` carrier plus `mini-hbut`'s QR
  URL parser/terminal-preSign shortcut. Asterism retains only strict,
  freshly-bound zeroizing QR material parsing and an immutable preparation of
  Ylim's exact actor-bound `stuSignajax` second step. The preparation exposes no
  query/send method and does not resolve the conflicting pre-sign chains or
  absent independent readback. Carrier parsing now also enforces one identity
  alias globally and keeps HTTPS `activeId|id` separate from Ylim's required
  `aid/source=15/Code` family, rejecting cross-carrier material mixtures.
- 2026-08-15: re-checked Ylim's ordinary `handleSign` chain at the fixed pin.
  Its source explicitly requires issuing `preSign` before a later sign record
  can be created and always calls it before variant dispatch. Asterism
  therefore treats that GET as a mutation-sequence operation requiring durable
  issue authority and ambiguous-outcome recovery, not as a free read or remote
  completion readback. The existing preparation and parser remain
  transport-free.
- 2026-08-15: refreshed all eight recorded donor default branches, tags and
  latest Releases. Revisions remain
  `9699e632b492`/`1589eac9c07c`/`7ed64ff547d3`/`64fb2f06e10c`/
  `f72619a0b369`/`890686a5e54f`/`14e1dfd9cf11`/`2b81f7b55a68`;
  tag counts are 45/0/0/62/0/321/0/29 and latest Releases remain
  `v3.1.4`/none/none/`v1.4.6`/none/`4.15.3`/none/`v2.5.0`.
  The latest `mini-hbut` issue concerns branding/Android build files, not the
  sign protocol.
- 2026-08-15: audited Ylim's complete WebIM path. The read-only
  `im.chaoxing.com/webim/me` page supplies `#myToken`, `#myTuid`, and `#myName`;
  delivered Easemob text messages carry `ext.attachment.att_chat_course`, with
  `atype=2`, `aid`, `courseInfo.courseid` and `courseInfo.classid` identifying
  a sign event. Asterism now has an unregistered, context-bound Native
  bootstrap GET plus zeroizing credential and event-identity parsers. It does
  not open the Easemob connection. Ylim's listener has no message-ID/cursor or
  de-duplication persistence, recursively reconnects on close and disables
  delivery acknowledgements after two SDK reconnects; those details are
  recorded as shared subscription Core gaps. Ylim's `sign_history` was
  separately traced to local Prisma rows and is explicitly excluded as remote
  verification.
- 2026-08-15: retained Ylim's exact `#statuscontent` and `stuSignajax` response
  vocabulary in bounded, zeroizing, preparation-bound pure parsers. Empty
  pre-sign status, exact completed text, `success`, already-signed text and
  `success2` remain controlled preflight/Receipt facts with response digests;
  unknown text fails closed. This adds no send path and deliberately does not
  treat an endpoint response as independent account completion readback.
- 2026-08-15: compared Samueli `741d5c1884aa84ea50c650897c870229c1de812e`
  with both newly pinned sign-in sources at the parameter level. The audit
  established shared field overlap but not a complete shared sequence; the
  current corrected implementation freezes Ylim's exact actor- and
  snapshot-bound `preSign -> stuSignajax` family with no send method rather
  than combining donor-specific fields.
  Gesture/code prerequisites, location field sets, photo upload/object identity
  and QR dynamic `enc` are not consistent or self-contained enough to prepare;
  all remain explicit blockers rather than guessed request shapes.
- 2026-08-15: added and fully pinned the current MIT `Ylim314/chaoxing-sign`
  PortSource at `7ed64ff547d352708066ff61f1b1dc1fb1be32f1` and GPL-3.0-or-later
  `superdaobo/mini-hbut` Reference at
  `64fb2f06e10c95a77a39f17d45c6e2b573ad63a2`. Their complete sign-in modules,
  types, tests, README/configuration surface, default branches and license
  metadata were inspected. Both corroborate `student/activelist` and
  `newsign/signDetail`; their conflicting status/variant interpretations are
  now an explicit fail-closed boundary. Asterism adds only unregistered,
  read-only, course-bound list/detail parsing and Native GET transport with
  synthetic fixtures. It does not call `preSign` or `stuSignajax`.
- 2026-08-15: re-read Samueli sign-in commit `741d5c1`, learning-count commit
  `38a2698` and OCS's pinned hyperlink/timed-reader/in-video implementation.
  The sign-in CLI flag is unwired and its low-level request returns opaque text;
  learning-count advances only local state without server readback; hyperlink
  and timed Read prove Browser actions plus waits but no completion; and random
  in-video answers remain excluded. Recorded exact durable/readback/fixture
  blockers without dropping these donor protocol surfaces from scope.
- 2026-08-15: no CxKitty revision changed while the shared durable
  interactive-auth contract landed. Wired the existing QR transport into the
  Development factory with canonical encrypted continuation state, exact
  revision/sequence binding, result digests, typed waiting/rejected/expired
  outcomes, an intermediate identity-validation phase which never re-polls
  remote success, separate Provider revision/Core poll-sequence recovery after
  released retryable claims, and crash-recoverable deterministic terminal
  Cookie finalization.
- 2026-08-15: re-read the pinned CxKitty/OCS/agent/`chaoxing-exam` type-2
  paths without a donor revision change. Added explicit page-derived blank
  cardinality, limited Chapter/Exam mutation to one bound blank/value, and
  fixture-covered exact single-blank Chapter history/standard evidence plus
  Exam submitted-value verification. Pending labels and multi-blank shapes
  remain unsupported instead of being concatenated or inferred.
- 2026-08-15: completed the Provider-owned partial mobile Exam follow-up from
  the full sweep without changing donor revisions. The encrypted attempt schema
  is now v3: it preserves the complete ordered Question binding and selected
  original positions, sends CxKitty's full-paper `start` index for each selected
  save, and verifies only selected visible answers after requiring the complete
  result count. Synthetic non-contiguous selection/result coverage is recorded
  in `FIXTURES.md`.
- 2026-08-15: re-read the complete pinned CxKitty QR call chain after the
  workspace checkpoint. No donor revision changed. Added a clean-room bounded
  Native QR begin/poll boundary and synthetic state/Cookie fixtures; live
  validation of the 2024 route remained pending.
- 2026-08-15: refreshed all six donor default branches, tag counts, latest
  Releases and issue updates after the partial-coverage Core checkpoint. Every
  recorded revision remains unchanged; tag counts remain 45/0/0/321/0/29,
  latest Releases remain `v3.1.4`/`4.15.3`/`v2.5.0` where present, and no issue
  was updated after the prior 2026-08-15 refresh. Re-reading Samueli's complete
  Work form and OCS's thresholded browser submit confirmed that unanswered
  controls remain in the complete form while the threshold is computed over
  all Questions. Asterism now rebuilds empty unanswered Work/Chapter Work
  fields from a fresh editor instead of forwarding stale or invented values.
- 2026-08-15: refreshed all six donor default branches; every recorded revision
  remains unchanged. The Audio checkpoint re-audited Samueli's shared
  status/report/signature flow and Audio Referer/`dtype`, plus OCS's exact
  `property.module=insertaudio|insertvideo` card distinction. Asterism uses the
  structural distinction and deliberately rejects Samueli's undifferentiated
  Video-failure-to-Audio fallback.
- 2026-08-14: completed the mandatory one-time full upstream sweep in
  `FULL_UPSTREAM_SWEEP.md`: complete default-branch trees, README/config,
  implementation, tags/Releases, issues/examples and known fixtures were
  re-enumerated for all six donors. Revisions remain unchanged. The sweep
  recovered partial-coverage policy, extended observed Question types,
  historical result bootstrap, retake/score-improvement, Audio, sign-in,
  learning-count and in-video Question scope; shared blockers and
  provider-private/live-fixture boundaries are recorded explicitly.
- 2026-08-14: `Samueli924/chaoxing` advanced from `dee643fd0a8e` to
  `9699e632b492` on `main` via merge PR #621. The delta is limited to external
  AI/Tiku connectivity checks and Python cleanup; it changes no audited
  Chapter, Resource or Live request contract. Its latest tag remains `v3.1.4`.
  The other five recorded donor default revisions remain unchanged.
- 2026-08-14: refreshed all six recorded donor default `HEAD` revisions at the
  durable Exam-start checkpoint. Every revision remains unchanged. OCS still
  resolves its default `HEAD` to branch `4.0` at `890686a5e54f`; its unrelated
  `main` and `master` pointers are not treated as default-branch updates.
- 2026-08-13: refreshed every recorded donor through the GitHub API. All six
  recorded default-branch revisions remained unchanged. Tags and releases were
  also enumerated: `Samueli924/chaoxing` latest tag/release remains `v3.1.4`,
  OCS latest release is `4.15.3`, and the historical `cxmooc-tools` release is
  `v2.5.0`; the other three donors currently expose no tags or releases. These
  revisions are reproducible audit snapshots, not a freeze; refresh again at
  the next capability-family checkpoint.
