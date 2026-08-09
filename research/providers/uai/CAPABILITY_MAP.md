# UAI capability map

| Asterism capability | Primary evidence | Use | Current decision |
|---|---|---|---|
| Authentication | AutoFinish + UnipusHelperPro | Reference | Password/ImportedToken orchestration, strict login-envelope classification, native Password exchange and atomic openid/JWT CompositeSession are offline/native-boundary covered |
| Stored session validation | User-info and Course-list reads | FromScratch | Core provider-scoped resolver accepts only exact unexpired native Composite or manual JWT metadata; native Composite can renew atomically after user-info validation, manual JWT cannot |
| CourseInventory | AutoFinish + UnipusHelperPro | Reference | Native authenticated Course list plus bounded Course → CourseResource flattening are offline/native-boundary covered |
| TaskInventory | AutoFinish + UnipusHelperPro | Reference | Native fresh resource-detail/tree reads plus bounded nested Course JSON → Unit/Section/Node/Group parser are offline/native-boundary covered |
| TaskProgressRead | Both backend donors | Reference | Native fresh-detail + signed per-Unit read and identity-bound Group parser are offline/native-boundary covered; only pass/pass2/perm all 1 maps to completed |
| DurationRead | UnipusHelperPro | Reference | Summary exposes `finishProgress` and `duration` independently at Course/Unit level; unit/live meaning pending |
| ResourceExecution | AutoFinish | Reference | Direct completion/submission is not accepted as duration-complete execution |
| DurationReport | AutoPlayer | Reference | Current evidence is page residence and interaction, not a confirmed public HTTP reporter; deferred |
| Result verification | Fresh tree/progress/duration reads | FromScratch | Require independent readback after any future mutation |
| BrowserBridge | AutoPlayer | Reference | Possible duration compatibility path, but first-batch Capture-dependent work is deferred |

## Initial implementation boundary

The current crate advertises Authentication, CourseInventory, TaskInventory and
TaskProgressRead. It:

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
10. retains raw Group duration without assigning a unit and makes no
    DurationRead/Report, execution, submission, BrowserBridge or Capture claim.
