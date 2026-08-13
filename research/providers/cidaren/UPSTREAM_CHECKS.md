# Cidaren upstream checks

Cidaren donor revisions are reproducible audit snapshots, not permanent pins.
Before and after each meaningful Provider checkpoint, re-read both donors'
default-branch refs, tags/releases and intervening commits. If a ref changes,
record the old and new revision plus a bounded diff summary here before
updating `SOURCES.md`, `DONOR_DIFFERENCES.md`, protocol code or tests.

## 2026-08-13 reopened-source audit

| Upstream | Default branch snapshot | Tags/releases | Change assessment |
|---|---|---|---|
| `ularch/Easy_Cidaren` | `master@bce9559f536ebbdad791f41ed4e111b30accb05d` | Latest tag/release remains `1.5.4@7e29ee43692f4c0807fae8cf7f74a5a674793097`, published 2025-12-09 | Compared with the former project snapshot `f2b5c25`: `c8de5f4` reopens executable source and adds token-only local Capture, public `jv=3_2265/3_2277` plus an alternate `3_1021` transform, progress UI and a post-run score read; `bce9559` documents reopening. Platform call sites otherwise remain the known token/account, class/study inventory, word evidence and five mutation routes |
| `MOPELotus/Easy_Cidaren` | `master@a74b4a2c1cdc5d38f568d41ccb11bb4d441ee4f1` | No published GitHub release at check time | No branch movement since the existing 2026-05-13 snapshot. It remains the additive Composite Capture, current encrypted word lookup and authenticated `jv=99` evidence line |

The latest ularch release asset is historical relative to its reopened source:
`Easy_Cidaren1_5_4.zip`, 103462537 bytes, SHA-256
`526011a4ccd14cc38887a663d54a5c78c33d8ea3d48e6c424a32128b6d0d8aca`.
The asset was identified from GitHub release metadata and is not redistributed
or treated as newer than `master`.

### Incremental implementation result

- added exact bounded public `jv=3_2265` and `jv=3_2277` decoders;
- retained both divergent exact `3_1021` transforms with unique/equal-result
  acceptance and ambiguity rejection;
- retained MOPE/current `jv=99` HKDF/AES-GCM instead of falling back to the
  public legacy decoder;
- accepted public-donor token-only Capture at the Provider validation and
  stored-session boundaries and implemented its distinct request-header
  recipe, while retaining Composite Capture and native OAuth material;
- retained the post-run score as an independent fresh verification fact;
- integrated the ordered multi-recipe Core contract as soon as it became
  available; actual local helper execution and executable BrowserBridge remain
  shared gaps rather than reasons to drop either donor path.

Post-implementation ref check: `ularch/master` remained `bce9559`,
`MOPELotus/master` remained `a74b4a2`, ularch latest release remained `1.5.4`
and MOPELotus still exposed no GitHub release. No additional diff entered this
checkpoint between the initial audit and the 99-test Provider verification.

### 2026-08-13 exact task-score checkpoint

Pre-implementation ref check again resolved `ularch/master` to full revision
`bce9559f536ebbdad791f41ed4e111b30accb05d` and `MOPELotus/master` to
`a74b4a2c1cdc5d38f568d41ccb11bb4d441ee4f1`. The latest ularch release/tag
remained `1.5.4@7e29ee43692f4c0807fae8cf7f74a5a674793097`, and MOPELotus still
published no GitHub release. There was therefore no upstream diff to port
before this checkpoint.

The reopened public donor's new `main_api.get_task_score` was then audited as
its own protocol route rather than approximated from PageTask alone. Asterism
now implements exact `ClassTask/Info` and `StudyTask/Info` query shapes,
account-bound read headers, bounded plain/legacy/current decoding,
conflict-checked `score/task_score/grade`, precise thousandth-point conversion
and SubmissionVerify integration. The public donor's dedicated requests
session does not copy `UserToken`; that is treated as a donor implementation
defect, not a platform requirement. Its study query also omits stable
`list_id`, so a fresh `task_id=-1` unit retains its identity-bound
`StudyTask/List` score rather than issuing an ambiguous request.

Post-implementation fetch resolved the same two full revisions and the same
`1.5.4` tag target; no new donor diff entered while the 105-test Provider,
all-target clippy and scoped diff checks completed.

### 2026-08-13 persisted native-OAuth validation checkpoint

The pre-implementation fetch again resolved `ularch/master@bce9559` and
`MOPELotus/master@a74b4a2`, with ularch release/tag `1.5.4` unchanged. The
OAuth V2 handoff documents remained the same imported manifest. No donor or
handoff delta preceded this incremental authentication audit.

The audit found that initial and Core pre-commit NativeProviderLogin checks
used the current OAuth V2 User-Agent/derived `Abc`/`Authorization-v: 00`
context, while a later stored-session validation discarded the non-secret
acquisition distinction and fell back to legacy donor headers. The resolved
Cidaren session now retains only that validation-context enum alongside its
zeroizing token/crypto material; NativeProviderLogin reuses the current OAuth
readback path, while manual and Capture origins remain on their evidenced
donor-header path.

Post-implementation fetch retained the same two donor revisions and ularch
tag target. The 106-test Provider suite, all-target clippy and scoped diff
checks completed without a new upstream delta.

## Check procedure

For the next checkpoint:

1. resolve each repository's symbolic `HEAD` and full default-branch SHA;
2. list new tags and releases, including asset digest/size metadata when a
   packaged release could carry source-visible behavior;
3. diff the prior snapshot to the new revision by executable call site,
   protocol constant, authentication/Capture path and response transform;
4. update the capability/protocol difference tables before porting code;
5. run Provider tests, all-target clippy and scoped diff checks;
6. re-read both refs immediately before declaring the checkpoint complete.
