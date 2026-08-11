# Cidaren research sources

Audit date: 2026-08-11. Revisions are frozen before implementation; moving
default branches and release binaries are not evidence for this checkpoint.

| Source | Revision | Capability evidence | Use | License |
|---|---|---|---|---|
| [`MOPELotus/Easy_Cidaren`](https://github.com/MOPELotus/Easy_Cidaren) | `a74b4a2c1cdc5d38f568d41ccb11bb4d441ee4f1` | Current imported `UserToken`, account validation, class Course/task inventory, learning/test execution, response obfuscation and 2026 `jv=99` crypto/capture changes | PortSource | GPL-3.0 |
| [`ularch/Easy_Cidaren`](https://github.com/ularch/Easy_Cidaren) | `f2b5c25c0811b0d409f7ad5a8221305fbe847329` | Public lineage, class task lifecycle, user-reported response fixtures and execution flow through the last source-visible generation | Historical | GPL-3.0 |
| [`github123666/cidaren`](https://github.com/github123666/cidaren) | `1409858800f3c4bd27577a08049bf1f8d17a069c` | Original protocol lineage, historical WeChat-code exchange, `UserToken` validation, task routes and signing | Historical | MIT |

The private donor is the primary current behavior source supplied by the
repository owner. It is one protocol/crypto update ahead of the public branch
and is audited directly rather than inferred from the GUI. The public issue
tracker supplies independently recorded class-task fields and lifecycle facts,
not credentials or executable request material:

- [issue 6](https://github.com/ularch/Easy_Cidaren/issues/6) records paginated
  `ClassTask/PageTask` rows, `task_type`, `over_status`, progress, score and
  millisecond timing fields;
- [issue 106](https://github.com/ularch/Easy_Cidaren/issues/106) records the
  identity hazard around `task_id=-1`, so a class task must be rebound through
  its release identity before later operations;
- [issue 107](https://github.com/ularch/Easy_Cidaren/issues/107) records the
  2026 encrypted-response drift that the private donor addresses.

No live account was contacted during this audit. Capture-dependent bootstrap is
explicitly deferred by the first-batch decision.
