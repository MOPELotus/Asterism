# Upstream provenance

Provider development in Asterism is upstream-first. Before implementing a
provider, add every donor and research source here and in
`research/providers/<provider>/`.

| Provider | Source | Revision | Capability | Use | License | Audited | Live validation |
|---|---|---|---|---|---|---|---|
| `chaoxing` | [`MOPELotus/chaoxing-evolved`](https://github.com/MOPELotus/chaoxing-evolved) | `0db3113ffd9f` | Auth, Session, Course, Chapter, Resource, Chapter Work | Reference | GPL-3.0 | 2026-08-09 | Pending |
| `chaoxing` | [`surinrasu/CxKitty`](https://github.com/surinrasu/CxKitty) | `1589eac9c07c` | Auth, Course, Chapter, Exam, Question, Submission, Errors | Reference | GPL-3.0 | 2026-08-09 | Pending; revision is from 2024 |
| `chaoxing` | [`iwillwill-ALLWILL/chaoxing-agent-skill`](https://github.com/iwillwill-ALLWILL/chaoxing-agent-skill) | `f72619a0b369` | Independent Work/Exam inventory, status, verification | PortSource | MIT | 2026-08-09 | Donor-reported only; Asterism pending |
| `chaoxing` | [`ocsjs/ocsjs`](https://github.com/ocsjs/ocsjs) | `890686a5e54f` | Current Work/Exam routes and Browser behavior | Reference | MIT | 2026-08-09 | Pending |
| `chaoxing` | [`LangHY/chaoxing-exam`](https://github.com/LangHY/chaoxing-exam) | `14e1dfd9cf11` | Chapter-test navigation, DOM and verification | Reference | No license found | 2026-08-09 | Donor-reported only; Asterism pending |
| `chaoxing` | [`CodFrm/cxmooc-tools`](https://github.com/CodFrm/cxmooc-tools) | `2b81f7b55a68` | Historical Work/Exam Browser routes | Historical | MIT | 2026-08-09 | Not applicable |

`Use` must be one of `PortSource`, `Reference`, `Historical`, or `FromScratch`.
