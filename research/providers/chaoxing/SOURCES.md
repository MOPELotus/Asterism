# chaoxing upstream sources

Audit date: 2026-08-13

This is a static source audit. No donor or Asterism implementation was live-tested
against a real account during this audit, so every live-validation claim remains
pending.

| Source | Revision | Updated | Use | Audited implementation surface | Live status |
|---|---|---|---|---|---|
| [`Samueli924/chaoxing`](https://github.com/Samueli924/chaoxing) | `dee643fd0a8e47e2b9ebeefc6515ff8c5acba49a` | 2026-07-22 | Reference | Password and Cookie login, Cookie validation, course/folder inventory, Chapter cards, Video, Document, Chapter Work | Pending |
| [`surinrasu/CxKitty`](https://github.com/surinrasu/CxKitty) | `1589eac9c07c4bab71f79d762b45210643dd537d` | 2024-09-29 | Reference | Web password/QR login, SSO session validation, mobile course/Chapter APIs, independent Exam inventory, Exam/Chapter Work detail and submission, typed error branches | Pending; protocol age is a risk |
| [`iwillwill-ALLWILL/chaoxing-agent-skill`](https://github.com/iwillwill-ALLWILL/chaoxing-agent-skill) | `f72619a0b36996d27d00577015663ec39e782500` | 2026-06-17 | PortSource | Browser-session Work and Exam inventory/status, submittability classification, result verification, current DOM reliability rules | Donor reports real use; Asterism validation pending |
| [`ocsjs/ocsjs`](https://github.com/ocsjs/ocsjs) | `890686a5e54f9a6d52d1169bae9ea5971e0863c7` | 2026-07-01 | Reference | Current Work/Exam page route taxonomy, question-page behavior and Browser lifecycle | Pending |
| [`LangHY/chaoxing-exam`](https://github.com/LangHY/chaoxing-exam) | `14e1dfd9cf11cd54dabb494dd01e318856d9b8d3` | 2026-06-19 | Reference | Chapter-test iframe navigation, current question DOM and post-submit verification pitfalls | Donor reports Chapter-test coverage; Asterism validation pending |
| [`CodFrm/cxmooc-tools`](https://github.com/CodFrm/cxmooc-tools) | `2b81f7b55a68ea2ceb6ea0312a6791ebf3ed3dc5` | 2021-11-03 | Historical | Older browser routes and Work/Exam question handling | Not current enough for implementation authority |

## Source selection

- Use the MIT-licensed agent skill as the primary behavior source for independent
  Work/Exam discovery, status classification, and result verification.
- Use the current OCS branch to cross-check Browser routes and DOM behavior, not
  as a task inventory implementation.
- Use `Samueli924/chaoxing` for the existing Chapter path and account/session behavior.
- Use CxKitty to understand the complete mobile API call chain and typed failure
  branches, but do not assume its 2024 transport still works in 2026.
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
the `toauthlogin` URL, and poll `getauthstatus`. That is the next Provider-owned
authentication candidate, but challenge presentation, owner-bound polling and
atomic credential commit remain shared runtime responsibilities.

## Refresh log

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
