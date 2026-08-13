# Cidaren drift register

| Risk | Static evidence | Required response |
|---|---|---|
| Imported token format changes | Current donor treats `UserToken` as opaque | Validate only bounded header safety; do not require historical hex length |
| Token expires after another login | Donor warns another device login can refresh token | Classify authenticated-read rejection as Authentication, then acquire a fresh manual or Capture-assisted composite session |
| WeChat OAuth/bootstrap changes | Current workflow depends on WeChat H5 and browser storage | Keep a visible origin-bounded BrowserBridge/Capture path; fail closed when the browser recipe or captured credential shape drifts |
| Historical native WeChat exchange is copied as current | Original `LoginByWechatCode` signs a stale hard-coded code while sending the runtime code | Treat it as non-reproducible lineage; require coherent live signature evidence before adding native OAuth, while retaining the implemented Capture route |
| Account validation returns HTML/redirect | Donor assumes JSON | Shared native client disables redirects, bounds the body and requires JSON before parsing |
| Class-task pagination is partial | Donor loops pages from `total` | Native transport fetches page one first; parser requires a complete, ordered, total-consistent page set before returning any Courses/Tasks |
| Body and signing versions are accidentally unified | Current donor sends `231204` but signs `240122` | Freeze the split in an exact-time request vector and treat any future change as protocol research |
| Course title changes across rows | Course data is duplicated on every task row | Require one normalized title per stable Course in a scan; treat conflicts as protocol drift |
| Selected Course changes or `StudyTask/List` returns another Course | `Student/Main` supplies the ordinary study query binding | Re-read the selected Course, bind it into the request and reject response/row identity disagreement |
| `task_id=-1` or stale ID targets another task | Public issue 106 and donor flow | Use `release_id` as stable identity; TaskDetail/Progress already fresh-rebind through a complete scan and every later operation must do the same |
| Ordinary unit `task_id` remains `-1` | Public issue 83 records repeated uninitialized study rows | Use `course_id + list_id` as the stable ordinary-study identity and keep `task_id` observational |
| Stable release disappears between reads | Remote inventory can change independently of local state | Return RemoteChanged from the fresh TaskDetail/Progress scan instead of serving stale detail or progress |
| New `task_type` appears | Current evidence recognizes class 1/2 and ordinary study 3 | Fail closed within the matching response family and add a sanitized fixture before normalization |
| Status vocabulary changes | Donor documents `over_status` 1/2/3 | Preserve unknown only after explicit mapping decision; never silently mark executable |
| Completion is inferred from expiry | Donor filters expiry and progress independently | Keep remote state, progress and close status separate |
| `time_spent` is assumed to be seconds | Samples are millisecond-like but no live unit proof exists | Preserve raw value; do not expose DurationRead until measured |
| Response `jv` obfuscation changes | Multiple inserted-byte variants and 2026 `jv=99` exist | Decode only exact frozen legacy variants; require the fresh account-bound Capture context for authenticated HKDF/AES-GCM `jv=99`; fail closed on unknown `jv` |
| Browser crypto material leaks | Current donor captures login/session crypto context | Store only through SecretStore as a Composite session; zeroize parsed JSON/key/plaintext and never persist it in Task, Question, Draft or logs |
| `StartAnswer` is treated as a read | Donor creates/advances a remote answer attempt before yielding the current topic | Use Core's durable AttemptStart boundary; never hide this non-idempotent mutation inside QuestionInventory or replay an ambiguous start |
| A stale `topic_code` crosses Questions | Verify/advance responses rotate the current topic code | Keep the code in redacted zeroizing attempt route state and bind every Verify/Submit/Skip to the same durable attempt and fresh Question |
| Matching relations are submitted with one stale token | Donor updates `topic_code` after every `VerifyAnswer` | Issue exactly one relation, accept its decoded rotated code, then issue the next; never batch or replay the sequence |
| `SubmitChoseWord` is parsed as a Question response | Donor validates it but does not decode a next step | Use the separate acknowledgement-only parser and require decoded data for Start/Verify/advance |
| Random timing changes across recovery | Donor chooses delay and reported duration from ranges | Resolve immutable bounded settings and derive a stable value for the bound attempt step; Core schedules delay and persists issued mutation facts |
| Mutation success text is accepted as verification | Donor stops on localized completion strings | Re-list exact release identity and verify fresh progress/state after mutation |
| Development Provider activates accidentally | No live account verification exists | Keep registration disabled by default and metadata at Development |

## Live-validation gate

After the product UI/plugin stage and account delivery:

1. verify imported-token validation, expiry classification and re-import;
2. compare complete class-task pagination, selected-Course study units and
   merged Course grouping with visible H5;
3. confirm task type/status/access vocabulary, `task_id=-1` behavior and both
   stable release/list identities;
4. measure `start_time`, `over_time` and `time_spent` semantics without remote
   mutation;
5. record only sanitized response fixtures and retain Development until all
   applicable live gates pass.
6. exercise Capture/BrowserBridge and `jv=99` with ephemeral crypto material;
7. validate mutation families only under explicit live-test authorization,
   preserving attempt/receipt/verification separation.
