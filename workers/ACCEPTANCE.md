# 0.0.1 Worker acceptance record

This file separates reproducible local fixture evidence from live-account
evidence. A green fixture run never implies that a donor account was reached.

## Reproducible local evidence

Run from the repository root:

```powershell
py -3.14 workers/run_tests.py
python workers/deployment_acceptance.py
```

The standard-library runner loads each provider's existing `unittest` suite by
path and does not import donor credentials. The current baseline is 42 tests
when the declared Chaoxing parser dependencies are installed. If
`beautifulsoup4`/`lxml` are absent, only the rich-HTML parser cases fail with
an explicit dependency error; classify that as an environment gap, not a
provider protocol regression.

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
- WELearn/UAI: real account read-only course/task inventories are present in the
  deployment database; completion-rate and duration mutation probes remain
  optional live validation because the audited donor paths are fixture-covered.
- Cidaren: real WeChat OAuth and read-only Unit/class-task inventory are present.
  Timed Instant mutation remains live validation. Any future first-question
  probe must explicitly authorize the donor's attempt-starting read.
- AI: real endpoint calls remain optional deployment validation; all request
  shapes, fallback, cache, usage and billing behavior are locally covered.

Reports must contain counts, kinds, timing and error classes only. Never store
course titles, question text, answers, cookies, JWTs, tokens or native payloads.
