# UAI capability map

| Asterism capability | Primary evidence | Use | Current decision |
|---|---|---|---|
| Authentication | AutoFinish + UnipusHelperPro | Reference | Password JSON login returns openid/JWT; parser/native transport pending |
| Stored session validation | User-info and Course-list reads | FromScratch | Validate JWT with bounded authenticated reads before persistence; pending |
| CourseInventory | AutoFinish + UnipusHelperPro | Reference | Fixture-only Course → CourseResource flattening implemented; native read pending |
| TaskInventory | AutoFinish + UnipusHelperPro | Reference | Fixture-only nested Course JSON → Unit/Section/Node/Group parser implemented; native read pending |
| TaskProgressRead | Both backend donors | Reference | Per-Unit `course_progress` exposes independent Group state; parser pending |
| DurationRead | UnipusHelperPro | Reference | Summary exposes `finishProgress` and `duration` independently at Course/Unit level; unit/live meaning pending |
| ResourceExecution | AutoFinish | Reference | Direct completion/submission is not accepted as duration-complete execution |
| DurationReport | AutoPlayer | Reference | Current evidence is page residence and interaction, not a confirmed public HTTP reporter; deferred |
| Result verification | Fresh tree/progress/duration reads | FromScratch | Require independent readback after any future mutation |
| BrowserBridge | AutoPlayer | Reference | Possible duration compatibility path, but first-batch Capture-dependent work is deferred |

## Initial implementation boundary

The current crate advertises no runtime capability. It only:

1. parses a bounded authenticated Course list and flattens each CourseResource
   into a stable `RemoteCourse`;
2. binds a fresh Course-resource detail to the selected resource without
   serializing `courseInstanceId`;
3. decodes the bounded nested `course` JSON string and emits stable Group Tasks
   with separate Unit, Section and Micro hierarchy facts;
4. drops class, instance, content, answer and unknown fields;
5. makes no authentication, progress, completion, duration, execution,
   submission, BrowserBridge or Capture claim.
