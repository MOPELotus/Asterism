# Cidaren capability map

| Asterism capability | Primary evidence | Use | Current decision |
|---|---|---|---|
| Authentication | Current private donor | PortSource | Accept only manually imported opaque `UserToken`; native shared-client validation through bounded `Student/Main` is offline/native-boundary covered; no Password or native WeChat OAuth claim |
| Stored session validation | Current private donor + Asterism secrets boundary | FromScratch | Core-scoped resolver accepts one exact unexpired ManualImport + ProviderSpecific access token bound to account/reference/purpose; no trustworthy remote expiry is exposed |
| Session recovery / refresh | Current private donor | Reference | Token replacement requires user import in the non-Capture batch; automatic refresh is not advertised |
| CourseInventory | Current/private task pages + public issue fixture | PortSource | Native signed pagination plus unique stable Course derivation from `course_id` and `course_name` are offline/native-boundary covered |
| TaskInventory | Current/private donor | PortSource | Native signed `ClassTask/PageTask` pagination is all-or-nothing; learning/test types remain distinct and stable identity binds to `release_id` |
| TaskDetail | Current donor + Asterism Core | FromScratch | Freshly re-list and require one exact release identity before returning details; never trust a stale `task_id` |
| TaskProgressRead | Current/private task rows | PortSource | Fresh task row exposes bounded percent, status, score and time observations; completion, duration and score remain separate |
| DurationRead | Public issue fixture | Reference | `time_spent` is recorded but its unit and semantics require live proof; do not expose seconds yet |
| QuestionInventory / QuestionParse | Current private donor | PortSource | `StartAnswer` returns one decoded topic at a time; defer until the current `jv` decoder and attempt lifecycle are isolated |
| AnswerResolve | Current private donor | PortSource | Donor derives answers from course vocabulary and topic mode; keep separate from parsing and mutation |
| SubmissionBuild | Current/private donor | Reference | Preserve signed request shape as a credential-free preview before execution |
| SubmissionExecute | Current/private donor | PortSource | `VerifyAnswer`, `SubmitAnswerAndSave`, `SkipAnswer` and `SubmitChoseWord` are distinct mutations; deferred until read-only chain is complete |
| SubmissionVerify | Fresh class task/detail read | FromScratch | A mutation receipt or completion message is insufficient; require fresh exact release readback |
| Capture bootstrap | Current private donor | Reference | Deferred; no mitmproxy/system-proxy or injected browser-storage capture in the first batch |

## Current implementation boundary

The initial non-Capture milestone now:

1. establish a Development-only Provider crate with exact canonical ID
   `cidaren`;
2. validate one bounded, redacted and zeroized manually imported `UserToken`;
3. classify `Student/Main` success, ordinary token expiry and malformed
   responses without retaining account PII;
4. parse all-or-nothing class-task pages into unique Courses and stable Tasks;
5. normalize learning tasks as routine Practice and test tasks as routine Exam,
   without treating either source type as a formal assessment;
6. retain progress, score and raw timing independently;
7. use `class-task:{release_id}` as the stable Task identity and preserve
   `task_id` only as a fresh observation;
8. composes injected Authentication, CourseInventory and TaskInventory slots
   into one registry-consistent Development entry;
9. resolves only an exact Core account/reference-bound manual access token and
   uses one shared non-redirecting HTTPS client for account validation and
   signed complete task pagination;
10. freezes donor headers, request/body signing-version split, JSON/status/body
   bounds and sensitive `UserToken` handling with offline tests;
11. advertises no execution, answer, duration-seconds, Capture or live-verified
   capability.
