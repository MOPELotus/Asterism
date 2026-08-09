# UAI capability map

| Asterism capability | Primary evidence | Use | Current decision |
|---|---|---|---|
| Authentication | AutoFinish + UnipusHelperPro | Reference | Password/ImportedToken orchestration, strict login-envelope classification and atomic openid/JWT CompositeSession are offline-covered; native transport pending |
| Stored session validation | User-info and Course-list reads | FromScratch | Injected account-bound resolver and JWT validation boundary implemented; Core storage/native read/renewal pending |
| CourseInventory | AutoFinish + UnipusHelperPro | Reference | Fixture-only Course → CourseResource flattening implemented; native read pending |
| TaskInventory | AutoFinish + UnipusHelperPro | Reference | Fixture-only nested Course JSON → Unit/Section/Node/Group parser implemented; native read pending |
| TaskProgressRead | Both backend donors | Reference | Per-Unit `course_progress` exposes independent Group state; parser pending |
| DurationRead | UnipusHelperPro | Reference | Summary exposes `finishProgress` and `duration` independently at Course/Unit level; unit/live meaning pending |
| ResourceExecution | AutoFinish | Reference | Direct completion/submission is not accepted as duration-complete execution |
| DurationReport | AutoPlayer | Reference | Current evidence is page residence and interaction, not a confirmed public HTTP reporter; deferred |
| Result verification | Fresh tree/progress/duration reads | FromScratch | Require independent readback after any future mutation |
| BrowserBridge | AutoPlayer | Reference | Possible duration compatibility path, but first-batch Capture-dependent work is deferred |

## Initial implementation boundary

The current crate advertises only injected Authentication. It:

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
6. drops class, instance, content, answer and unknown fields;
7. makes no native-network, progress, completion, duration, execution,
   submission, BrowserBridge or Capture claim.
