# 0.0.1 Worker acceptance record

This file separates reproducible local fixture evidence from live-account
evidence. A green fixture run never implies that a donor account was reached.

## Reproducible local evidence

Run from the repository root:

```powershell
py -3.14 workers/run_tests.py
```

The standard-library runner loads each provider's existing `unittest` suite by
path and does not import donor credentials. The current baseline is 40 tests:

| Provider | Fixture scope | Live account required |
|---|---|---:|
| Chaoxing | inventory projection, rich question parsing, answer payload shape, verification guards | No |
| WELearn | course/SCO inventory, completion-rate and duration mapping, auth redaction | No |
| UAI | worker JSON protocol, fixture upstream, content/answer mapping, failure redaction | No |
| Cidaren | unit/class inventory, OAuth/token boundaries, native answer evidence and delay settings | No |

## Live evidence still required

- Chaoxing: a real authenticated account must complete a read-only scan after
  the current `provider_unavailable` dead-letter cursor resumes; no submission
  is required for this acceptance.
- WELearn/UAI: real account read-only course/task and completion-rate/duration
  probes remain deployment validation; the donor paths are fixture-covered.
- Cidaren: real WeChat OAuth callback and a read-only Unit/class-task probe
  remain deployment validation. Class-task probing must explicitly authorize
  the donor's attempt-starting first-question read.
- AI: real endpoint calls remain optional deployment validation; all request
  shapes, fallback, cache, usage and billing behavior are locally covered.

Reports must contain counts, kinds, timing and error classes only. Never store
course titles, question text, answers, cookies, JWTs, tokens or native payloads.
