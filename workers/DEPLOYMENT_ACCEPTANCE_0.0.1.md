# Asterism 0.0.1 deployment acceptance

This report is intentionally credential-free. It distinguishes local evidence
from live account evidence and is safe to keep in the repository.

## Local acceptance

- Branch is `0.0.1`; `master` and `0.1.0` remain unchanged.
- Daemon health: `ok`; database schema: 101; four Providers registered; no
  outbox backlog or dead-letter outbox entries.
- Full Rust workspace: green, including API 70, Engine 158, Storage 170,
  Chaoxing 204, WELearn 255, UAI 308, Cidaren 214, and all remaining crates.
- Python Worker fixture smoke: 42/42 green with declared Chaoxing HTML parser
  dependencies installed.
- Yunzai plugin: `npm test` 10/10 and `npm run check` green.
- WebUI production build/typecheck and the major route/mobile/error-state pass
  are complete.
- `python workers/deployment_acceptance.py` emits only aggregate, credential-free
  health and inventory facts. The current deployment proves all four stored
  accounts authenticated and records these live read-only inventories:
  Chaoxing 17 courses / 5,330 tasks, Cidaren 2 / 31, UAI 2 / 1,250 and WELearn
  4 / 1,594. No course title, question, answer or native payload is selected.
- QQ formal notifications: deadline-before confirmation and deadline-missed
  draft-preserved stages are persisted, claim/report scoped, idempotent and
  retryable.
- Chaoxing challenge contract: only answer points retry, maximum three times,
  then one `sol_xhigh` escalation marker; no repeated video/live/document
  mutation.

## Live validation still required

- Chaoxing: resume the existing conservative full-scan cursor after its
  `provider_unavailable` dead-letter and validate CAPTCHA/Exam verification,
  challenge unlock and observed long-tail question families.
- WELearn/UAI: read-only account/course/task inventory is deployment-validated;
  a real mutation probe of their separate completion/duration paths remains
  optional because the accepted upstream behavior and local wire fixtures are
  the 0.0.1 usability evidence.
- Cidaren: WeChat OAuth and read-only Unit/class-task inventory are
  deployment-validated; timed Instant mutation/fallback observation remains.
- AI/pricing: optional real endpoint call and a deployment-local fee settlement
  check after administrators configure endpoints/rates.
- Yunzai: one real target instance/group message send and delivery report.

These are environment/account checks, not unimplemented control-plane
features. No live validation report may include titles, questions, answers,
cookies, JWTs, tokens or native payloads.
