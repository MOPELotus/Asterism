# Cidaren research sources

Audit date: 2026-08-14. Revisions below are reproducible snapshots for this
checkpoint, not permanent pins. Default branches, tags/releases and key
commits are rechecked before and after each meaningful Provider checkpoint;
changes are recorded in [`UPSTREAM_CHECKS.md`](UPSTREAM_CHECKS.md) before an
incremental audit/port/test cycle.

The mandatory one-time README/config/default-branch/tag/release/issue/example/
implementation/fixture re-audit is recorded in
[`FULL_UPSTREAM_SWEEP.md`](FULL_UPSTREAM_SWEEP.md). It supersedes any earlier
assumption that the pinned-revision delta alone represented the complete donor
surface.

| Source | Revision | Capability evidence | Use | License |
|---|---|---|---|---|
| [`MOPELotus/Easy_Cidaren`](https://github.com/MOPELotus/Easy_Cidaren) | `a74b4a2c1cdc5d38f568d41ccb11bb4d441ee4f1` | Current imported `UserToken`, account validation, class Course/task inventory, learning/test execution, response obfuscation and 2026 `jv=99` crypto/capture changes | PortSource | GPL-3.0 |
| [`ularch/Easy_Cidaren`](https://github.com/ularch/Easy_Cidaren) | `bce9559f536ebbdad791f41ed4e111b30accb05d` | Current public lineage, class task lifecycle, execution flow and current-payload topic progress counters, token-only local Capture helper and exact `jv=3_1021/3_2265/3_2277` transforms | Reference | GPL-3.0 |
| [`ularch/Easy_Cidaren` release 1.5.4](https://github.com/ularch/Easy_Cidaren/releases/tag/1.5.4) | tag commit `7e29ee43692f4c0807fae8cf7f74a5a674793097`; asset SHA-256 `526011a4ccd14cc38887a663d54a5c78c33d8ea3d48e6c424a32128b6d0d8aca` | Latest published packaged release (2025-12-09), retained as a provenance checkpoint because it predates the 2026-08-12 reopened default-branch source | Historical release | GPL-3.0 project release; binary not redistributed |
| [`github123666/cidaren`](https://github.com/github123666/cidaren) | `1409858800f3c4bd27577a08049bf1f8d17a069c` | Original protocol lineage, historical WeChat-code exchange, `UserToken` validation, task routes and signing | Historical | MIT |
| User-supplied Cidaren H5 asset snapshot | SHA-256 `65b9c80f2dbc0775fb61813f89f254128c03d7dca928b4af8694bff4fe61fefe` | First-party `2.7.0.260715_01` OAuth callback, P-256 bootstrap, V2 exchange, HKDF/AES-GCM and browser session behavior | Reference | Protocol observation only; not redistributed |
| User-supplied redacted WeChat flow capture | SHA-256 `0d937e9c621429bac6cc6e1892e43a9e5758dff8e79c4f5d56d35f052c199ec7` | Live-safe callback, exact V2 request/response envelope, new refresh-cookie issuance and fresh `Student/Main` success with the old refresh cookie removed | Reference | Sanitized protocol evidence only; not redistributed |
| User-supplied PC WeChat XWeb research snapshot | SHA-256 `883d48ee513cd03397f95734363c7482a00bc4c267fa8c0a6fda09d1d53f7c8e` | XWeb remote-debug/CDP protocol, callback acquisition research and the unresolved provisioning boundary | Reference | Protocol observation only; not redistributed |
| [User-supplied OAuth V2 assisted-bootstrap handoff](handoffs/2026-08-13-oauth-v2/IMPORT.md) | Archive SHA-256 `5c09f74d5c73df6339ad4daac48f51dff218f10219c9cdf417d9f2aa12384f70` | Random-marker OAuth callback preservation, strict one-shot callback handoff, current V2 exchange, and authorized Windows/Android device-flow validation | Reference | Documentation and manifest imported; raw evidence, probes and reference code remain external |

The owner-supplied donor is the primary current automation behavior source and
is audited directly rather than inferred from the GUI. The public lineage has
since diverged: it retains a token-only local proxy helper and adds exact legacy
`jv=3_*` transforms, while the owner-supplied donor carries the current
Composite Capture and authenticated `jv=99` path. Both evidenced surfaces are
in scope. The first-party H5
snapshot and sanitized live-safe capture supersede the historical native login
sample: they establish a coherent current V2 code exchange without copying
first-party assets into the repository. The historical original was rechecked
at its frozen revision; its stale hard-coded code is lineage only. The public issue
tracker supplies independently recorded class-task fields and lifecycle facts,
not credentials or executable request material:

- [issue 6](https://github.com/ularch/Easy_Cidaren/issues/6) records paginated
  `ClassTask/PageTask` rows, `task_type`, `over_status`, progress, score and
  non-zero millisecond `time_spent`/timing fields;
- [issue 83](https://github.com/ularch/Easy_Cidaren/issues/83) records a 2026
  `StudyTask/List` response with selected Course metadata, ordinary unit rows,
  `task_type=3`, `list_id`, access flags and repeated `task_id=-1`;
- [issue 43](https://github.com/ularch/Easy_Cidaren/issues/43) records a 2025
  `StudyTask/Info` response using the legacy `jv=2_1254` inserted-byte base64
  family; the published payload is evidence only and is not copied into a
  fixture;
- [issues 48](https://github.com/ularch/Easy_Cidaren/issues/48) and
  [49](https://github.com/ularch/Easy_Cidaren/issues/49) repeatedly record the
  same definite five-field incomplete-section `SubmitChoseWord` rejection. A
  sanitized response-only fixture now freezes that exact public envelope; the
  Provider classifies and retains its digest/time without copying any Task,
  account, word-map or log context. Shared blocked Question-step
  representation remains a Core Gap;
- [issue 72](https://github.com/ularch/Easy_Cidaren/issues/72) records the
  exact remote-state response `code=20001`, `msg=需要选词！`, `data=null` from
  `StartAnswer`; release 1.5.2 reported a fix even though the reopened shared
  handler still expresses a truthy-data success condition;
- [issue 99](https://github.com/ularch/Easy_Cidaren/issues/99) and its public
  diagnostic attachment establish an unimplemented `topic_mode=73` shape:
  paired integer Task row identity, two answers, two bounded word lengths and
  no options. Only structural, redacted facts were extracted; no raw log,
  account content, real Task identity or topic code is retained;
- [issues 70](https://github.com/ularch/Easy_Cidaren/issues/70),
  [77](https://github.com/ularch/Easy_Cidaren/issues/77) and
  [85](https://github.com/ularch/Easy_Cidaren/issues/85) contain multi-step
  decoded Question logs. Value-free equality/cardinality checks establish one
  response Task allocation while topic tokens rotate; no raw value is retained;
- [issue 106](https://github.com/ularch/Easy_Cidaren/issues/106) records the
  identity hazard around `task_id=-1`, so a class task must be rebound through
  its release identity before later operations;
- [issue 107](https://github.com/ularch/Easy_Cidaren/issues/107) records the
  2026 encrypted-response drift that the private donor addresses.

The Rust implementation and synthetic tests do not contact a live account.
The separately supplied redacted capture is sufficient protocol evidence for
the V2 exchange and cookieless result, but does not replace future Asterism
end-to-end validation. Live Provider validation remains pending for account
binding, helper callback delivery, inventory and authorized mutation flows.

The 2026-08-13 assisted-bootstrap handoff refines the earlier XWeb-only
acquisition hypothesis: using an independent random `authorize_marker != "2"`
prevents the Cidaren frontend from consuming the callback, so an explicitly
returned callback URL can feed the native V2 exchange without MITM or XWeb
debug provisioning. Its source manifest verified all 16 archive entries before
the documentation slice was imported. Provider-side URL construction,
hash-only binding, strict callback parsing and the native exchange are now
implemented. The shared hash-only owner/account/AuthSession-bound pending
record, atomic claim/consume, Provider callback exchange and credential commit
are also implemented; end-to-end live validation remains pending.

The shared BrowserBridge runner now consumes Cidaren's strict command artifact,
reads only the Provider-declared request-header/local/session-storage sources
and emits the exact typed terminal result. Its deliberately temporary Chromium
profile does not inherit authenticated PC WeChat XWeb storage or reproduce the
public donor's system-proxy/certificate lifecycle. That remaining acquisition
problem needs shared environment support or an authorized login plus live
validation; it is no longer an unimplemented Cidaren command/result boundary.

The complete side-by-side audit is frozen in
[`DONOR_DIFFERENCES.md`](DONOR_DIFFERENCES.md). In particular, the reopened
public source augments rather than supersedes the owner-supplied `jv=99` and
Composite Capture work, and neither Python branch supersedes the first-party
no-MITM OAuth V2 handoff.
