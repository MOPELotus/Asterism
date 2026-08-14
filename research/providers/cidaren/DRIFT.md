# Cidaren drift register

| Risk | Static evidence | Required response |
|---|---|---|
| Imported token format changes | Current donor treats `UserToken` as opaque | Validate only bounded header safety; do not require historical hex length |
| Token expires after another login | Donor warns another device login can refresh token | Classify authenticated-read rejection as Authentication, then acquire a fresh manual, random-marker OAuth or Capture-assisted Composite session |
| WeChat OAuth callback acquisition changes | Authorized Windows/Android probes establish manual return of a random-marker callback; pure Web iframe/popup relay and the earlier XWeb-only assumption do not hold | Generate independent CSPRNG state/marker (`marker != "2"`), persist only domain-separated hashes, accept only exact HTTPS Cidaren student callbacks in audited query/SPA forms, and never persist or replay the single-use code |
| Historical native WeChat exchange is copied as current | Original donor `LoginByWechatCode` signs a stale hard-coded code; the frozen first-party `2.7.0.260715_01` H5 and redacted live-safe V2 exchange establish the replacement | Use only the current P-256/SPKI, sorted MD5, ECDH, HKDF `vcg-auth-aes` and AES-GCM/AAD flow; fail closed on handshake/version/AAD drift and fresh-read the resulting account before persistence |
| OAuth exchange is retried after an ambiguous failure | WeChat callback code is single-use and the V2 POST mutates authentication state | Core must durably claim and consume an owner/account/AuthSession-bound artifact; Provider sends once and never retries a Network-ambiguous result |
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
| Post-run score is stale, unauthenticated or cross-unit | Reopened donor reads `score/task_score/grade` from class/study Info but its dedicated session omits `UserToken`, and the study query omits stable `list_id` | Rebind the Task first, authenticate with its account session, reject conflicting/out-of-range aliases, and retain the fresh list score instead of issuing an ambiguous study `task_id=-1` request; score never proves completion |
| `time_spent` is misread as seconds or unbounded | Public issue 6 has non-zero values, sibling time fields are milliseconds and mutation routes write the same field | Preserve raw milliseconds, bound to 100 years, fresh-rebind, then truncate to Domain seconds; retain live comparison as validation rather than an implementation blocker |
| Response `jv` obfuscation changes | Owner donor has fixed-index legacy variants and 2026 `jv=99`; current public donor additionally has sequential-span/five-chunk `3_1021`, `3_2265` and `3_2277` | Decode only exact frozen transforms; for conflicting `3_1021` donor evidence accept only a unique/equal JSON result; require fresh account-bound crypto from Capture or native V2 exchange for authenticated HKDF/AES-GCM `jv=99`; fail closed on unknown or ambiguous `jv` |
| Browser crypto material leaks | Current donor captures login/session crypto context | Store only through SecretStore as a Composite session; zeroize parsed JSON/key/plaintext and never persist it in Task, Question, Draft or logs |
| A recovered Capture result is rebound with caller-selected metadata | Core durably receives encrypted raw result bytes before Provider parsing | Resolve the persisted metadata and `SecretValue` together; recheck result type/session/sequence/raw digest/receive time against the issued command, validate the raw digest before interpreting UTF-8/JSON, and derive terminal completion time only from that receipt |
| `StartAnswer` is treated as a read | Donor creates/advances a remote answer attempt before yielding the current topic | Use Core's durable AttemptStart boundary; never hide this non-idempotent mutation inside QuestionInventory or replay an ambiguous start |
| Pre-Question recovery assumes Start always yields a Question | Eligible tasks may require `SubmitChoseWord`, and mode 0 yields a reading card before any real Question | Rotate an encrypted `cidaren.pre-question-attempt.v2` continuation through selection/start/reading-card phases, bind every phase to its exact bounded position and materialize a QuestionSession only after a real Question exists; reject position-less v1 state rather than guessing whether it had already advanced |
| Recovery identity includes a new audit correlation | Worker recovery legitimately creates another correlation while owner/account/Task stay fixed | Validate correlation syntax but exclude it from Provider flow identity; keep it only in the durable operation audit |
| A stale `topic_code` crosses Questions | Verify/advance responses rotate the current topic code | Keep the code in the encrypted, digest-covered continuation and rebind its local/remote Task, remote Question, position and content fingerprint before every Verify/Submit/Skip |
| Remote topic progress is mistaken for local attempt position | Reopened donor only renders `topic_done_num/topic_total` from the current payload; its state machine advances separately | Preserve a bounded optional completed/total observation on Question and reading-card steps, reject contradictory counters, and never use it as a mutation cursor or Question identity input |
| Matching relations are submitted with one stale token | Donor updates `topic_code` after every `VerifyAnswer` | Issue exactly one relation, accept its decoded rotated code, then issue the next; never batch or replay the sequence |
| Matching recovery forgets which relations were accepted | A rotated token alone cannot distinguish the second relation from a replay of the first | Encrypt a bounded monotonic `verified_steps` checkpoint in `cidaren.question-attempt.v2`; rebuild the deterministic relation queue from the immutable Draft and reject count/phase/selection disagreement |
| An internal attempt phase is consumed without replacement | Every mutation edge temporarily takes ownership of one phase before installing `Issued` or a restored error state | Expose a sanitized `Invalid` status, return a typed Internal error from every fallible continuation/issue path and keep Debug/read-only inspection non-panicking; never fabricate a replacement mutation phase |
| `topic_mode=73` is treated as an ordinary one-text completion | Public issue 99 shows two answers/two word lengths/no options and the donor explicitly reports its direct answer encoding unimplemented | Parse and expose a bounded FillBlank Question, reject AnswerResolve and ordinary answer encoding until a live or upstream trace establishes the multi-answer wire shape, but permit a distinct explicit Skip Draft/`SkipAnswer` path and verify only fresh Task completion facts |
| `SubmitChoseWord` is parsed as a Question response or every empty `20001` is treated alike | Donor validates it but does not decode a next step; shared handler requires truthy `data`, while public issue 72 establishes exact `20001 + 需要选词！ + null` remote state and `20004` is terminal | Use the separate receipt parser, accept only that exact empty-data exception as fresh-selection-required, terminate on `20004`, reject other false/empty `20001`, and require decoded data for ordinary Start/Verify/advance |
| Random timing changes across recovery | Donor chooses configurable question delay/reported duration plus fixed verified-answer 1-2 second and reading-card 1-3 second waits | Resolve immutable bounded settings and derive distinct stable values for the bound attempt step; the issued command exposes its pre-execution residence and Core persists it before scheduling, while Provider mutation code never sleeps |
| Runtime default bypasses donor timing floor | Reopened donor config defaults `min_time=max_time=2` and both UI controls enforce a 2-second minimum | Runtime schema revision 2 uses the same 2-second defaults/minima at Provider/account/Task scope; invalid legacy zero-delay patches fail schema validation instead of silently changing execution behavior |
| Mutation text overrides an unrelated failure code or is accepted as verification | Donor normally interprets localized strings inside successful responses; issue 72 establishes one exact `20001` remote-state exception | Require numeric/data success or the exact evidenced selection-required tuple before classifying text, then re-list exact release identity and verify fresh progress/state after mutation |
| Development Provider activates accidentally | No live account verification exists | Keep registration disabled by default and metadata at Development |

## Live-validation gate

After the product UI/plugin stage and account delivery:

1. verify imported-token validation, expiry classification and re-import;
2. compare complete class-task pagination, selected-Course study units and
   merged Course grouping with visible H5;
3. confirm task type/status/access vocabulary, `task_id=-1` behavior and both
   stable release/list identities;
4. compare the implemented millisecond `time_spent` conversion with visible H5
   duration and retain a sanitized non-zero study-row sample;
5. compare authenticated post-run class/study Info scores with the freshly
   rebound list row, including alias shape and any `task_id=-1` unit;
6. record only sanitized response fixtures and retain Development until all
   applicable live gates pass.
7. exercise Capture/BrowserBridge and `jv=99` with ephemeral crypto material;
8. validate mutation families only under explicit live-test authorization,
   preserving attempt/receipt/verification separation.
9. validate the assisted random-marker OAuth URL and manually returned callback
   through the native V2 exchange on the applicable Windows/Android paths,
   including cancellation, expiry, duplicate delivery and ambiguous-failure
   recovery, without retaining raw state, marker or code in a fixture or log.
