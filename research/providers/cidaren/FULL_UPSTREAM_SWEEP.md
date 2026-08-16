# Cidaren one-time full upstream sweep

Audit date: 2026-08-14. This is the mandatory full re-audit required by
`research/providers/NEXT_CHECKPOINT.md`. It enumerates the complete recorded
surface from scratch rather than considering only commits after the prior pin.

## Evidence enumerated

| Evidence line | Material re-read |
|---|---|
| `ularch/Easy_Cidaren` | Complete default-branch file tree, README, every config key, all Python implementation files, all 14 tags (`1.0.0` through `1.5.4`), all 14 GitHub Releases, the previously verified 1.5.4 packaged-asset inventory and all 105 non-PR issues plus relevant comments/attachments |
| `MOPELotus/Easy_Cidaren` | Complete default-branch file tree, README/config, all Python implementation files, inherited tags and the additive Composite Capture plus `jv=99` commit |
| `github123666/cidaren` | Complete historical default branch, README/config, all Python implementation files, all 10 tags (`v1.0` through `v3.73`) and all nine GitHub Releases |
| First-party H5/captures | Recorded H5 asset and redacted V2 capture manifests, protocol facts and hashes in `SOURCES.md` |
| PC WeChat XWeb snapshot | Recorded CDP/provisioning findings and immutable evidence hash |
| OAuth V2 handoff | Every imported manifest/document: protocol, security, device matrix, evidence index and implementation handoff |

The symbolic refs remained:

- `ularch/master@bce9559f536ebbdad791f41ed4e111b30accb05d`;
- `MOPELotus/master@a74b4a2c1cdc5d38f568d41ccb11bb4d441ee4f1`;
- `github123666/main@1409858800f3c4bd27577a08049bf1f8d17a069c`;
- public `1.5.4@7e29ee43692f4c0807fae8cf7f74a5a674793097`.

No donor contains an automated test or fixture directory. Its reusable
behavioral evidence is source code, inline sanitized response examples,
README screenshots, public issue payloads/logs and packaged-release bytecode.

## Complete executable protocol surface

| Donor operation | Current Asterism mapping | Classification |
|---|---|---|
| imported `UserToken`, historical one-shot WeChat code and current V2 OAuth | ImportedToken, assisted Capture and ExternalBrowserOauth with fresh `Student/Main` validation | Implemented; stale historical code exchange is provenance only |
| `Student/Main` | authentication validation and selected-Course binding | Implemented |
| `ClassTask/PageTask` | bounded complete Course/Task inventory with release identity | Implemented |
| `StudyTask/List` | selected-Course ordinary-unit inventory with `course_id + list_id` identity | Implemented |
| `StudyTask/Info` | task/word detail, answer inventory and post-run score when unambiguous | Implemented |
| `Resource/CoursePage/{course_id}.json` | credential-free self-built word inventory | Implemented |
| `Course/StudyWordInfo` | bounded meaning/phrase/example evidence | Implemented |
| `Course/SearchWord` | prototype alias fallback | Implemented |
| `SubmitChoseWord` | durable grouped-word-map one-shot pre-Question mutation plus exact issues 48/49 existing-selection rejection classification | Grouped `course_id:list_id -> words` maps match every eligible donor call site. The exact rejection on an ordinary-unit plan rotates to durable `ready-to-start` and fresh `StartAnswer` without replaying selection; self-built plans and near matches fail closed |
| `StartAnswer` | durable non-idempotent attempt start; class request selector is donor-fixed at `task_type=2` while the decoded current-step echo retains class row type 1/2 | Implemented; request selector and response-row identity are validated as separate facts |
| `VerifyAnswer` | one answer/relation per durable operation with rotated token | Implemented |
| `SubmitAnswerAndSave` | durable answer/reading-card advance | Implemented |
| `SkipAnswer` | distinct explicit Skip operation | Implemented |
| class/study `Info` score read | fresh independently bound SubmissionVerify score | Implemented |
| fixed `jv=2_*`, divergent `3_1021`, public `3_2265/3_2277`, owner `jv=99` | exact bounded decoder candidates and authenticated crypto context | Implemented |
| public token proxy and owner token/storage Capture | strict BrowserBridge command/read/result plus atomic credential terminal | Implemented through the shared runner; authenticated external context remains a live/shared-environment blocker |

Historical `v1.0/v1.1` defines `get_all_task`, but its body only assigns the
string `/Student/ClassTask/PageTask` and logs a message. It performs no request
and has no caller. The executable class inventory in every lineage is
`ClassTask/PageTask`; no legacy `Student/ClassTask` capability was omitted.

## Complete settings surface

| Donor setting/control | Semantics | Asterism mapping |
|---|---|---|
| `min_time`, `max_time` | real delay between accepted steps; current UI enforces at least two seconds | `answer.delay_min_seconds` / `answer.delay_max_seconds`, revision 2, default/minimum 2 |
| `spend_min_time`, `spend_max_time` | reported answer duration multiplied by 500 ms | `answer.reported_time_min_millis` / `answer.reported_time_max_millis`, donor defaults 2500/7500 ms |
| fixed Skip `time_spent=20000` | reported Skip duration | `answer.skip_reported_time_millis`, default 20000 |
| fixed 1-2 s verified-answer residence | wait before answer advance | stable command scheduling delay |
| fixed 1-3 s reading-card residence | wait before reading advance | stable command scheduling delay |
| `class_task`, `myself_task` and current manual task-type/name controls | select which discovered Tasks a run will execute | product authorization/scheduling over complete TaskInventory; not wire behavior |
| historical loop over every class/incomplete study Task | sequential batch composition | complete inventory plus durable per-Task execution; no Provider batch protocol |
| `br_choices`, `accept_encoding` | Python transport compatibility for Brotli/gzip | shared native HTTP decoding concern; no semantic runtime policy |
| `token`/saved token | credential material | SecretStore-bound session; never an ordinary runtime setting |
| `version`, `know_version`, `read` | updater and first-run UI state | application UI, not Provider protocol |
| `play_music`, `music_path` | completion notification | application UI, not Provider protocol |
| provider/account concurrency and scan interval | Asterism scheduling controls | explicit Core-owned runtime settings added by Asterism, not donor wire fields |

Issue 51 confirms cumulative task duration is not implemented by the donor;
only per-answer reported time exists. Issue 64's all-unfinished-unit request
and issues 68/69's additional self-study selection were explicitly deferred,
not executable donor features. Issue 54 confirms the two-second floor is
intentional anti-detection behavior, and issue 82 confirms the reported-time
settings are not intended to change answer correctness.

## Question and answer surface

The complete source and tag history recognizes reading mode 0 and answer modes
11, 13, 15-18, 21-22, 31-32, 41-44 and 51-54. Asterism maps all of those
families, including nested answer tags, matching relations, phrase/example
evidence, prototype resolution, completion heuristics and explicit low-
confidence donor fallbacks. Optional `topic_done_num/topic_total` remains a
remote observation rather than a local cursor.

## History, evidence, completion and retake boundary

The added owner-global Answer Evidence Corpus / Strict Completion / Score
Improvement design was audited separately against every donor route and
payload example:

| Shared concern | Cidaren protocol fact | Provider consequence |
|---|---|---|
| First-bind history bootstrap | No donor enumerates historical attempts, submitted answers, per-Question correctness or standard-answer results. `ClassTask/PageTask`, `StudyTask/List` and the two Info routes expose current Task/unit state and at most a task-level score | A read-only first-bind bootstrap can discover current/completed Tasks but has no Cidaren answer-history records to harvest into the corpus |
| Normal incremental evidence | `StartAnswer` exposes the current Question and `VerifyAnswer` returns only the rotated `topic_code`; the donor computes an answer from word/phrase/example evidence rather than reading platform correctness back | `AnswerResolve` may supply source-derived candidates, but `SubmissionVerify` must leave every Question `Unverified`; it cannot certify a corpus entry from task-level progress/score |
| Strict Completion | Class selection treats `progress < 100` as incomplete and terminal responses report task completion; Asterism's fresh read requires the internally consistent pair `Completed + 100%` | The Provider already supplies the exact read-only terminal fact needed by a shared default-on Strict Completion state machine. Score and expiry remain independent |
| Passing threshold | No README, config, issue, route or payload defines a passing score. The historical donor's `95+ correctness` text describes expected answer quality, not a platform pass rule | Do not infer pass/fail from score or install a Provider threshold |
| Score Improvement / retake | No donor exposes attempt history, remaining retake count, retake eligibility, a reset route or a distinct retake operation. Inline `chance_num` and `answer_state` belong to the current Question; donor reruns call `StartAnswer`, while their inventory filters completed class Tasks out | A shared Score Improvement state machine must report Cidaren retake availability as unknown/unsupported and must not start a completed Task speculatively |

The current ordinary-study selector includes rows only when `progress <= 97`,
whereas class selection uses `progress < 100`. Nothing else assigns terminal
meaning to 98/99, so this is a donor filtering quirk, not evidence for a 97%
completion or passing threshold. Asterism continues to treat 98/99 as
in-progress and confirms completion only at fresh 100%/Completed.

Public issue 99 supplies the structural mode-73 two-blank payload but the
donor maintainer explicitly states it is not adapted. Asterism parses the
bounded Question and implements the separately evidenced Skip path. No direct
multi-answer encoding can be added without a new upstream implementation or
authorized trace. Issues 15, 71 and 96 report unsupported/low-score Questions
but provide no additional sanitized mode or wire shape.

## Issues, examples, fixtures and application-only behavior

All 105 public issues were enumerated. Protocol-bearing clusters are already
represented by the existing research/fixtures:

- inventory/time/identity: issues 6, 43, 51, 82, 83 and 106;
- prerequisite/recovery: issues 48, 49 and 72;
- encoding drift: issues 23, 33, 34, 107, 108 and 111;
- Question drift: issues 15, 71, 96 and 99;
- Capture/login environment: issues 84, 87, 92, 101, 102, 109 and 113;
- device/network blocking: issues 97, 98, 100 and 112.

The remaining issues concern packaging, Python/model installation, GUI,
updates, logs or requests for unimplemented features. The default sources also
contain cancellation/UI progress, update checking, first-run notices, log
export, completion audio and device-ID display. The 1.5.4 package additionally
contains donor-operator telemetry/device blocking. None is a Cidaren learning-
platform protocol capability. The dormant Google Translate module has no
source caller and is absent from the packaged executable.

## Re-audit outcome

Every executable donor platform call and semantic runtime setting is either
implemented, intentionally represented by shared product scheduling/UI,
classified as non-platform behavior, or tied to an evidence-backed blocker.
No migration omission was found, so this checkpoint adds no speculative
Provider code or fixture. Existing synthetic fixtures remain the minimal
sanitized corpus; full Provider tests and strict Clippy verify the mapping.

The new shared Answer Evidence Corpus and Score Improvement state machines do
have an exact Main-owned integration boundary: Cidaren can report no
historical/per-Question verified evidence and no evidenced retake
availability. Strict Completion is already supported by fresh 100%/Completed
verification. Main must preserve these three independent states rather than
deriving corpus evidence or retake authority from task-level score.

The remaining blockers are the authenticated BrowserBridge context, real-
account protocol validation, explicitly authorized mutation validation and
mode-73 answer-wire evidence recorded in `CAPABILITY_MAP.md`. This one-time
full-sweep requirement is complete for Cidaren; ordinary future checkpoints
return to revision/delta auditing.
