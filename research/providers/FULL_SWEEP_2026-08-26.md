# Four-Provider full sweep — 2026-08-26

This is the mandatory next-checkpoint sweep required by `AGENTS.md` and
`NEXT_CHECKPOINT.md`. It is a local, credential-free audit of the recorded
donor manifests, worker entrypoints, runtime-setting schemas and current
fixtures. It does not claim live account validation.

## Chaoxing

- Recorded donors: Samueli924/chaoxing, CxKitty, chaoxing-sign, mini-hbut,
  chaoxing-agent-skill, ocsjs, chaoxing-exam and cxmooc-tools (see `UPSTREAMS.md`).
- Worker surface re-enumerated: password/session renewal, course and official
  chapter inventory, knowledge-point resources, independent work and exam
  inventory, question/result harvesting, save-only/formal submit, grade
  composition, sign-in reads, video/document/read/live and browser fallback.
- Current Asterism mapping: all listed executable paths have a Worker boundary
  or an explicit Provider-private/read-only classification. Runtime settings
  include video/answer/other concurrency, video rate, coverage threshold and
  bounded challenge retries.
- Newly checked delta: challenge markers are now recognized conservatively;
  only answer points retry (maximum 3), then emit one `sol_xhigh` escalation
  request. No video/live/document replay is performed by this policy.
- Remaining evidence: authenticated full scan continuation, CAPTCHA runtime,
  challenge unlock/account validation, and observed long-tail matching/compound
  submission on a real account.

## WELearn

- Recorded donors: Fanyuchang2026/welearn-helper, YZBRH/Welearn_helper and
  Auto_WeLearn (the latter historical).
- Worker surface re-enumerated: password/OIDC, course/unit/SCO inventory,
  CMI completion, independent duration lifecycle, singleton execution and
  bounded batch planning.
- Current Asterism mapping: completion and duration remain two separate donor
  calls; the runtime schema exposes correctness and duration independently.
  No Chaoxing-style answer bank or question submission path is imposed.
- Remaining evidence: live account validation and the authenticated public batch
  creation endpoint; no donor capability was removed during this sweep.

## UAI

- Recorded donors: AutoFinish_UxiaoyuanAI, UnipusHelperPro,
  UnipusAIAutoPlayer and UnipusAI.
- Worker surface re-enumerated: password/JWT/imported token, course/resource
  tree, required-unit completion, residence duration, discussion and typed
  media/oral/compound paths.
- Current Asterism mapping: per-account execution is serial; completion and
  duration remain distinct capabilities; cooldown settings are passed through
  both ordinary and private execution paths. Unsupported/unknown shapes fail
  closed rather than being guessed.
- Remaining evidence: live account/browser validation, media/oral long-tail
  paths and the authenticated batch endpoint.

## Cidaren

- Recorded donors: MOPELotus/Easy_Cidaren, ularch/Easy_Cidaren and the
  historical github123666/cidaren lineage.
- Worker surface re-enumerated: WeChat/OAuth bootstrap, imported token,
  course/task reads, answer/submission route, encrypted response handling and
  serial timing.
- Current Asterism mapping: OAuth remains the user-facing login path; the
  low-trust `answer_lib` is historical evidence only. Timed/untimed/escalation
  route and Instant timeout/grace settings are frozen and passed to both worker
  execution entries.
- Remaining evidence: live read-only worker route, timed fallback observation,
  and attachment/browser context validation.

## Cross-provider classification

All deltas observed in this sweep are classified as one of: implemented,
Provider-private, shared-Core follow-up, or live-validation-only. No donor
capability was deleted or silently disabled. The next implementation work is
therefore the remaining executable local gaps and live-validation harnesses,
not another broad architecture redesign.

