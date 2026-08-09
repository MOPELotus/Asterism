# UAI drift register

| Risk | Static evidence | Required response |
|---|---|---|
| Login result/code shape changes | Current donors use string code `0`; captcha uses `1506` | Parse typed bounded envelopes; classify challenges as `HumanRequired` and never log bodies |
| JWT header format changes | Donors pass the returned JWT directly as `Authorization` | Validate through user-info/Course reads; do not guess a Bearer prefix |
| JWT/openid are stored or renewed separately | Both values are required by different authenticated routes | Persist one strict ProviderCompositeSession and replace it atomically |
| Course list and detail disagree on instance fields | List has `instanceId`; detail has `courseInstanceId` | Use stable CourseResource ID and refresh/bind detail before tree reads |
| Course tree outer envelope changes | Current response stores JSON text in `course` | Bound both layers and fail closed on non-string, malformed or oversized nested data |
| New tree role appears | Audited roles are Unit, Section, Node, Link and Group | Reject unknown roles and add a sanitized fixture before mapping them |
| Group ID disappears or becomes URL-only | Historical parser falls back to URLs for generic nodes | Require explicit bounded Group IDs; never persist route URLs as Task identity |
| Completion flags gain new values | Current donor requires `pass == pass2 == perm == 1` | Keep each flag independent and map only sanitized verified combinations |
| Duration unit/meaning drifts | Summary exposes integer `duration` without a proven unit | Preserve raw value only until live before/after measurements establish semantics |
| Page residence stops producing duration | Current userscript relies on active page lifecycle | Prefer native network evidence; otherwise isolate a headless compatibility worker and verify fresh duration |
| Direct submit completes without duration | Backend donors submit answers independently of page residence | Keep CompletionService and DurationService separate; never claim execution from completion alone |

## Live-validation gate

Before advancing verification:

1. validate Password login, captcha classification, JWT expiry and recovery;
2. compare CourseResource and Unit/Section/Micro/Group trees with the visible
   account;
3. record per-Group flags plus Course/Unit completion and duration before and
   after ordinary study;
4. identify the actual heartbeat/telemetry lifecycle without Capture-dependent
   work in the first batch;
5. verify any future execution with independent fresh completion and duration
   reads;
6. commit only sanitized fixtures and a dated verification record.
