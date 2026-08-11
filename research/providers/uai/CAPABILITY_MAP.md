# UAI capability map

| Asterism capability | Primary evidence | Use | Current decision |
|---|---|---|---|
| Authentication | AutoFinish + UnipusHelperPro | Reference | Password/ImportedToken orchestration, strict login-envelope classification, native Password exchange and atomic openid/JWT CompositeSession are offline/native-boundary covered |
| Stored session validation | User-info and Course-list reads | FromScratch | Core provider-scoped resolver accepts only exact unexpired native Composite or manual JWT metadata; native Composite can renew atomically after user-info validation, manual JWT cannot |
| CourseInventory | AutoFinish + UnipusHelperPro | Reference | Native authenticated Course list plus bounded Course → CourseResource flattening are offline/native-boundary covered |
| TaskInventory | AutoFinish + UnipusHelperPro | Reference | Native fresh resource-detail/tree reads plus bounded nested Course JSON → Unit/Section/Node/Group parser are offline/native-boundary covered |
| TaskDetail | Asterism Core + both backend donors | FromScratch | Re-lists CourseResources and re-runs fresh detail/tree inventory before returning one exact identity-bound Group detail |
| TaskProgressRead | Both backend donors | Reference | Native fresh-detail + signed per-Unit read and identity-bound Group parser are offline/native-boundary covered; only pass/pass2/perm all 1 maps to completed |
| DurationRead | UnipusHelperPro | Reference | Native user-info + per-Unit study-record read binds CourseResource/Unit/Group exactly; donor-documented Task duration seconds are offline/native-boundary covered |
| Daemon composition | Asterism Core | FromScratch | Complete read-only Development entry is registered only through an independent disabled-by-default opt-in and configured SecretStore |
| QuestionInventory / QuestionParse | AutoFinish | Reference | Encrypted content route and task `base`/`question_num` are frozen; bounded decryption and typed parser remain the next offline slice |
| AnswerResolve | Both backend donors | Reference | Separate encrypted answer route is frozen; it must not be combined with question parsing or submission |
| SubmissionBuild | Both backend donors | Reference | Preserve only a credential-free, answer-value-free preview before any executable payload is formed |
| SubmissionExecute | Both backend donors | Reference | `newExploration/submit` is a distinct mutation; rate-limit responses are not receipts and no empty/upload/discussion default is inferred |
| SubmissionVerify | UnipusHelperPro | Reference | Fresh user-module/progress reads remain independent from the submit receipt and must bind the exact Group |
| ResourceExecution | AutoFinish | Reference | Direct completion/submission is not accepted as duration-complete execution |
| DurationReport | AutoPlayer | Reference | Current evidence is page residence and interaction, not a confirmed public HTTP reporter; deferred |
| Result verification | Fresh tree/progress/duration reads | FromScratch | Require independent readback after any future mutation |
| BrowserBridge | AutoPlayer | Reference | Possible duration compatibility path, but first-batch Capture-dependent work is deferred |

## Initial implementation boundary

The current crate advertises Authentication, CourseInventory, TaskInventory,
TaskDetail, TaskProgressRead and DurationRead. It:

1. parses a bounded authenticated Course list and flattens each CourseResource
   into a stable `RemoteCourse`;
2. binds a fresh Course-resource detail to the selected resource without
   serializing `courseInstanceId`;
3. decodes the bounded nested `course` JSON string and emits stable Group Tasks
   with separate Unit, Section and Micro hierarchy facts;
4. classifies bounded Password results, separates slider verification as
   `HumanRequired`, and validates Password or strict ImportedToken input;
5. stores openid/JWT together as one encrypted ProviderCompositeSession while
   retaining username/password only for future explicit renewal;
6. resolves only the account/reference-bound CompositeSession purpose and
   rejects mismatched origin, kind, expiry, identity or storage shape;
7. drops class, instance, content, answer and unknown fields;
8. implements native Password exchange, user-info JWT validation, complete
   Course/Task inventory and read-only per-Unit Group progress;
9. atomically renews only complete NativeProviderLogin+Composite credentials,
   with one Authentication-only retry and no ManualImport renewal;
10. re-lists CourseResources and rebuilds the exact Group from fresh
    detail/tree inventory for every TaskDetail read;
11. preserves bounded `base` task types and optional `question_num` while using
    a versioned Task fingerprint;
12. reads fresh bounded user identity and the exact per-Unit study-record tree,
    then returns seconds only for one unique identity-bound Group Task;
13. advertises ProgressRead and DurationRead on each normalized Group Task so
    Core can authorize the two independent read paths;
14. composes those six capabilities into an independently opted-in daemon
    Development entry that still requires the configured SecretStore;
15. retains progress-route Group duration without assigning a unit and makes no
    DurationReport, execution, submission, BrowserBridge or Capture claim.
