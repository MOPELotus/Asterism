# UAI capability map

| Asterism capability | Primary evidence | Use | Current decision |
|---|---|---|---|
| Authentication | AutoFinish + UnipusHelperPro | Reference | Password/ImportedToken orchestration, strict login-envelope classification, native Password exchange, standard JWT-expiry hint and atomic openid/JWT CompositeSession are offline/native-boundary covered |
| Stored session validation | User-info and Course-list reads | FromScratch | Core provider-scoped resolver accepts only exact unexpired native Composite or manual JWT metadata; expired native Composite can use its complete Password set only for atomic user-info-validated renewal, while manual JWT cannot renew |
| CourseInventory | AutoFinish + UnipusHelperPro | Reference | Native authenticated Course list plus bounded Course → CourseResource flattening are offline/native-boundary covered |
| TaskInventory | AutoFinish + UnipusHelperPro | Reference | Native fresh resource-detail/tree reads plus bounded nested Course JSON → Unit/Section/Node/Group parser are offline/native-boundary covered |
| TaskDetail | Asterism Core + both backend donors | FromScratch | Re-lists CourseResources and re-runs fresh detail/tree inventory before returning one exact identity-bound Group detail |
| TaskProgressRead | Both backend donors | Reference | Native fresh-detail + signed per-Unit read and identity-bound Group parser are offline/native-boundary covered; only pass/pass2/perm all 1 maps to completed |
| DurationRead | UnipusHelperPro | Reference | Native user-info + per-Unit study-record read binds CourseResource/Unit/Group exactly; donor-documented Task duration seconds are offline/native-boundary covered |
| Daemon composition | Asterism Core | FromScratch | Complete Development entry is registered only through an independent disabled-by-default opt-in and configured SecretStore |
| QuestionInventory / QuestionParse | AutoFinish | Reference | Fresh encrypted content read, bounded framing/decryption and typed single-choice, multiple-choice and short-answer parsing are offline/native-boundary covered |
| AnswerResolve | Both backend donors | Reference | A separate fresh encrypted standard-answer read is independently bound to the same Group/question snapshot; ciphertext, keys and route context are not returned |
| SubmissionBuild | Both backend donors | Reference | Credential-free and answer-value-free immutable preview is offline covered; no executable body is retained in the draft |
| SubmissionExecute | Both backend donors | Reference | Exact `newExploration/submit` mapping is enabled only for one-question simple Groups; `600001`/`600002` remain typed retry responses and never become receipts |
| SubmissionVerify | UnipusHelperPro | Reference | Exact receipt-versioned fresh user-module readback independently binds Course/Group/version/question/submitted state and answer; missing receipts are Inconclusive without a guessed route |
| ResourceExecution | AutoFinish | Reference | Direct completion/submission is not accepted as duration-complete execution |
| DurationReport | AutoPlayer | Reference | Current evidence is page residence and interaction, not a confirmed public HTTP reporter; deferred |
| Result verification | Fresh tree/progress/duration reads | FromScratch | Require independent readback after any future mutation |
| BrowserBridge | AutoPlayer | Reference | Possible duration compatibility path, but first-batch Capture-dependent work is deferred |

## Initial implementation boundary

The current crate advertises Authentication, CourseInventory, TaskInventory,
TaskDetail, TaskProgressRead, DurationRead, QuestionInventory/QuestionParse,
AnswerResolve and the three independent submission capabilities. It:

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
14. reads and decrypts fresh content and standard-answer documents separately,
    binds every normalized question and candidate to the exact stable remote
    Group Task snapshot,
    and drops ciphertext, key material and free-form route context;
15. builds only an answer-value-free immutable preview, then forms the native
    submission body inside the execution boundary for exactly one supported
    question (`single-choice`, `multichoice` or `short_answer`);
16. accepts only a successful version-bearing submit response as a receipt,
    returns ambiguous transport failures after one mutation attempt, and
    independently verifies that exact version through a fresh user-module read;
17. confirms only exact submitted-answer equality, returns Inconclusive when no
    version receipt exists, and never infers score, progress or completion;
18. composes these capabilities into an independently opted-in daemon
    Development entry that still requires the configured SecretStore;
19. retains progress-route Group duration without assigning a unit and makes no
    DurationReport, general TaskExecution, BrowserBridge, Capture or live
    compatibility claim.
