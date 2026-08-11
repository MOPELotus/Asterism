# Cidaren drift register

| Risk | Static evidence | Required response |
|---|---|---|
| Imported token format changes | Current donor treats `UserToken` as opaque | Validate only bounded header safety; do not require historical hex length |
| Token expires after another login | Donor warns another device login can refresh token | Classify authenticated-read rejection as Authentication and require re-import before Capture exists |
| WeChat OAuth/bootstrap changes | Current workflow depends on WeChat H5 and browser storage | Keep Capture deferred; never claim native Password or OAuth refresh |
| Account validation returns HTML/redirect | Donor assumes JSON | Shared native client disables redirects, bounds the body and requires JSON before parsing |
| Class-task pagination is partial | Donor loops pages from `total` | Native transport fetches page one first; parser requires a complete, ordered, total-consistent page set before returning any Courses/Tasks |
| Body and signing versions are accidentally unified | Current donor sends `231204` but signs `240122` | Freeze the split in an exact-time request vector and treat any future change as protocol research |
| Course title changes across rows | Course data is duplicated on every task row | Require one normalized title per stable Course in a scan; treat conflicts as protocol drift |
| `task_id=-1` or stale ID targets another task | Public issue 106 and donor flow | Use `release_id` as stable identity and fresh-rebind every later operation |
| New `task_type` appears | Current donor recognizes only 1 learning and 2 test | Fail closed and add a sanitized fixture before normalization |
| Status vocabulary changes | Donor documents `over_status` 1/2/3 | Preserve unknown only after explicit mapping decision; never silently mark executable |
| Completion is inferred from expiry | Donor filters expiry and progress independently | Keep remote state, progress and close status separate |
| `time_spent` is assumed to be seconds | Samples are millisecond-like but no live unit proof exists | Preserve raw value; do not expose DurationRead until measured |
| Response `jv` obfuscation changes | Multiple inserted-byte variants and 2026 `jv=99` exist | Bind decoder to exact version/crypto context; fail closed on unknown `jv` |
| Browser crypto material leaks | Current private donor captures login/session crypto context | Store only through SecretStore if Capture is later implemented; never persist in Task data or logs |
| Mutation success text is accepted as verification | Donor stops on localized completion strings | Re-list exact release identity and verify fresh progress/state after mutation |
| Development Provider activates accidentally | No live account verification exists | Keep registration disabled by default and metadata at Development |

## Live-validation gate

After the product UI/plugin stage and read-only account delivery:

1. verify imported-token validation, expiry classification and re-import;
2. compare complete class-task pagination and Course grouping with visible H5;
3. confirm task type/status vocabulary, `task_id=-1` behavior and stable
   release identity;
4. measure `start_time`, `over_time` and `time_spent` semantics without remote
   mutation;
5. record only sanitized response fixtures and retain Development until all
   applicable live gates pass.
