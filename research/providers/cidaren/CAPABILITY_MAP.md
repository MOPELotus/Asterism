# Cidaren capability map

| Asterism capability | Primary evidence | Use | Current decision |
|---|---|---|---|
| Authentication | Current private donor | PortSource | Accept only manually imported opaque `UserToken`; native shared-client validation through bounded `Student/Main` is offline/native-boundary covered; no Password or native WeChat OAuth claim |
| Stored session validation | Current private donor + Asterism secrets boundary | FromScratch | Core-scoped resolver accepts one exact unexpired ManualImport + ProviderSpecific access token bound to account/reference/purpose; no trustworthy remote expiry is exposed |
| Session recovery / refresh | Current private donor | Reference | Token replacement requires user import in the non-Capture batch; automatic refresh is not advertised |
| CourseInventory | Current/private task routes + public issue fixtures | PortSource | Native signed class pagination and selected-Course `StudyTask/List` are merged by stable `course_id`; conflicting titles fail the complete inventory |
| TaskInventory | Current/private donor + public issue 83 | PortSource | Native `ClassTask/PageTask` pagination is all-or-nothing; class learning/test identities bind to `release_id`, while ordinary study units bind to `course_id + list_id` because `task_id` may remain `-1` |
| TaskDetail | Current donor + Asterism Core | FromScratch | Offline/native-boundary covered: freshly re-list the matching class or study endpoint and require one exact stable identity before returning details; never trust a stale `task_id` |
| TaskProgressRead | Current/private task rows | PortSource | Offline/native-boundary covered for class and ordinary study Tasks: fresh rows expose bounded percent and state; duration remains unavailable while raw time, score and completion stay separate |
| DurationRead | Public issue fixture | Reference | `time_spent` is recorded but its unit and semantics require live proof; do not expose seconds yet |
| QuestionInventory / QuestionParse | Current private donor + public issue 43 | PortSource | Exact bounded legacy `jv=0`, `2_*` and `3_*` response decoding is isolated and offline-tested, but no capability is advertised because current `jv=99` still requires deferred Capture crypto context and `StartAnswer` creates an attempt lifecycle |
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
6. read the account-selected Course plus ordinary `task_type=3` study units
   through bounded `StudyTask/List`, merging Course evidence without hiding
   title conflicts;
7. retain progress, score, access flags and raw timing independently;
8. use `class-task:{release_id}` for class Tasks and
   `study-task:{course_id}:{list_id}` for ordinary study Tasks, preserving
   `task_id` only as a fresh observation;
9. composes injected Authentication, CourseInventory, TaskInventory,
   TaskDetail and TaskProgressRead slots into one registry-consistent
   Development entry;
10. resolves only an exact Core account/reference-bound manual access token and
   uses one shared non-redirecting HTTPS client for account validation and
   both task families;
11. freezes donor headers, request/body signing-version split, selected-Course
    query binding, JSON/status/body bounds and sensitive `UserToken` handling
    with offline tests;
12. re-lists the matching native inventory for every detail/progress read,
    rejects malformed or disappeared stable identities and never converts raw
    `time_spent` into duration seconds;
13. isolates exact legacy base64/confusion-byte response decoding without
    copying public word/question payloads, rejects unknown variants and returns
    UnsupportedTask for Capture-bound `jv=99`;
14. advertises no execution, question, answer, duration-seconds, Capture or
    live-verified capability.
