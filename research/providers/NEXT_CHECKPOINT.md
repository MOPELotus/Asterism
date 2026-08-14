# Next Provider checkpoint: full upstream re-audit and Chaoxing answer coverage

This document is a mandatory input for the next meaningful Provider checkpoint.
It records accepted follow-up work discovered after the first-batch Provider
implementation had already become broad enough to expose omissions in the
original audits.

## 1. Mandatory full upstream sweep for every first-batch Provider

At the next meaningful checkpoint, re-audit **all first-batch Providers**:

- Chaoxing
- WELearn
- UAI
- Cidaren

This pass is intentionally broader than the normal incremental upstream check.
Do not only inspect commits made after the pinned revision. Re-enumerate each
recorded donor's current README, configuration surface, default branch,
tags/releases, relevant implementation code, issues/examples and known
fixtures so that capabilities or settings which existed before the original pin
but were accidentally omitted are found as well.

For each Provider:

1. enumerate the donor-visible feature/settings surface from scratch;
2. compare it with Asterism's current capability map and runtime settings;
3. classify every delta as implemented, intentionally Provider-private, shared
   Core Gap, live-validation-only, unsupported due to missing protocol evidence,
   or an actual migration omission;
4. update `SOURCES.md`, `PROTOCOL.md`, `CAPABILITY_MAP.md`, `DRIFT.md` and
   `FIXTURES.md` where applicable;
5. record the newly audited upstream revision(s);
6. add regression fixtures/tests for any newly recovered behavior;
7. continue implementation instead of merely reporting the gap when the
   protocol can be established.

The Chaoxing `submit` + `cover_rate` setting is the motivating example: it is an
existing donor behavior that was not represented in Asterism even though the
rest of the submission chain had already been ported.

## 2. Chaoxing partial-answer submission / coverage threshold

`Samueli924/chaoxing` exposes `submit` plus `cover_rate`; the donor may submit
when the fraction of Questions for which it found answers reaches the configured
threshold rather than requiring an answer for every Question. Asterism's current
Chaoxing `SubmissionBuild` instead requires one selected answer for every parsed
Question, so this donor behavior is currently missing.

Migrate the semantic behavior without copying donor implementation shortcuts:

- expose a Master-owned runtime policy for the minimum answer coverage required
  before submission; preserve the audited donor default of `0.9` for Chaoxing
  unless a stronger product default is deliberately chosen elsewhere;
- calculate coverage against the complete current `QuestionSnapshot`, not only
  against Question kinds Asterism already knows how to encode;
- permit unanswered/unsupported Questions to remain unanswered when the
  configured threshold is satisfied and the remote submission protocol itself
  permits partial answers;
- do not turn `Unknown`, unsupported or missing-answer Questions into invented
  values merely to satisfy the threshold;
- keep the immutable Draft, request digest, receipt/readback separation and
  no-ambiguous-replay guarantees for the subset actually submitted;
- if the donor's prerequisite/unlock behavior requires a lower-coverage submit,
  model that as an explicit Core/Provider execution policy rather than silently
  bypassing the configured threshold.

A rare unsupported Question kind must therefore lower answer coverage, not
necessarily invalidate the entire otherwise-submittable Work/Exam.

## 3. Observed Chaoxing Question surface is the submission completion boundary

Chaoxing currently recognizes a wider Question type surface than it can submit.
In particular, Matching, Ordering and Composite families are recognized in the
parser/domain mapping but do not yet have a complete Chaoxing submission path.

For Chaoxing live validation, perform a bounded first-party account sweep across
all safely readable courses and historical Work / Chapter Work / Exam surfaces.
Collect sanitized protocol observations for every actually encountered Question
family, including shared-option and nested/compound shapes.

Completion rule:

> Every human-answerable Chaoxing Question family observed in audited donor
> fixtures or first-party live validation must have either a complete native
> submission/verification path or the smallest bounded BrowserBridge fallback.

Purely theoretical/unobserved type codes do not permanently block Provider
completion. They must still fail closed at runtime if encountered later and
should trigger sanitized drift capture for a future fixture rather than guessed
submission encoding.

Prefer Native HTTP when the exact donor wire can be audited. BrowserBridge may
serve as a long-tail fallback for UI-expressible interactions such as drag/drop,
matching or other rendered controls when a stable native mutation protocol
cannot yet be established.

## 4. Result-page answer harvesting must cover every reliably visible type

The current Chapter Work result parser extracts exact ProviderNative standard
answers only for single-choice, multiple-choice and true/false. This is an
implementation checkpoint, not the intended final scope. When the result page
visibly exposes a reliable standard/reference answer for another observed type,
add a typed parser for it rather than excluding the type merely because the
first parser covered only `0/1/3`.

Target normalized forms include, when donor evidence supports them:

- choice -> `Selections`;
- true/false -> `Boolean`;
- fill blanks -> `Texts`;
- short/reference answer -> bounded text evidence with provenance that preserves
  whether the donor labels it as a unique correct answer or a reference answer;
- matching -> `Pairs`;
- ordering -> `Ordering`;
- compound/shared-context tasks -> per-child evidence rather than one opaque
  string whenever the donor exposes per-child grading.

Do not infer a standard answer from total score alone. Per-Question evidence is
required to promote a submitted answer to verified answer evidence.

## 5. Answer evidence corpus and account bootstrap scan

Treat historical result harvesting as a reusable answer-evidence feature, not
only as immediate SubmissionVerify output.

The shared design should preserve at least these evidence classes separately
from the original answer source:

- **Official / ProviderNative**: the platform explicitly exposes the standard or
  correct answer;
- **VerifiedHistorical**: an answer originally produced by AI, an external bank,
  manual input or another source was actually submitted and the platform later
  provided per-Question evidence that it was correct;
- **Unverified**: an answer candidate has not yet received platform grading
  evidence.

Do not conflate evidence quality with `AnswerSource`; source provenance and
verification status are independent facts. Existing `ProviderNative`,
`LocalCache`, `ExternalBank`, `Manual`, etc. should remain meaningful.

Add a bounded **read-only answer harvest / bootstrap scan** concept for a
first-party account. It may enumerate accessible historical completed tasks and
collect:

- normalized Question content/context;
- platform standard/reference answers when displayed;
- the user's submitted answer;
- per-Question correct/incorrect state when displayed;
- score;
- retake availability/provenance.

The harvest job must not create a new attempt or mutate a task merely to obtain
answers. Its output can seed the local answer corpus before normal automation is
used.

## 6. Configurable retake / score-improvement policy

Chaoxing result/retake facts should support an explicit opt-in policy rather
than causing `SubmissionExecute` to silently create another attempt.

A retake is a new remote attempt and must have its own fresh `QuestionSnapshot`,
Draft, attempt authority, mutation ledger and verification.

The useful policy is **per Question**, not "all Questions must already have an
official answer":

- if the previous answer was wrong and an official standard answer is known,
  replace it with the official answer;
- if the previous answer was correct, that submitted value can be reused as
  `VerifiedHistorical` evidence even when no official answer was shown;
- if the previous answer was wrong and no better official evidence exists,
  resolve again through the configured answer sources/AI and compare evidence;
- do not require the new attempt to consist entirely of official answers;
- apply the ordinary answer-coverage threshold before the retake submission;
- expose explicit controls such as enabled/disabled, target score and bounded
  maximum retake count rather than an unbounded retry loop.

Retake recovery must never replay an ambiguous remote attempt-creation or
submission mutation.

## 7. Shared-context, shared-option and compound Question modeling

The current Domain already has `QuestionKind::Composite` and
`NormalizedAnswer::Composite`, but `Question` itself has no first-class child
Question structure or shared option/context object. UAI already carries richer
child semantics Provider-privately. Before more Providers encode such shapes in
opaque metadata, Main should evaluate a shared representation for:

- a passage/material/shared stem;
- shared images/audio/video/other attachments;
- an optional shared option set;
- ordered child Questions with independent kinds and per-child grading;
- nested/compound Questions where donor evidence truly requires hierarchy.

Reading comprehension is normally a shared context plus ordinary child
Questions, not a new leaf `QuestionKind`. Likewise, a common-option exercise is
best represented as children referring to one shared option set.

This work should also evaluate a conservative semantic answer-match fingerprint
for historical reuse across retakes. The existing exact
`QuestionContentFingerprint` intentionally hashes the ordered option structure;
a retake may reorder option labels while preserving semantic content. Historical
reuse must match semantic option content/context first and then remap the old
answer to the current option identity. Never reuse a cached letter such as `B`
when only the option order changed.

## Checkpoint exit criteria

The next Provider checkpoint is not complete until the four-Provider full
upstream sweep above has been recorded and any discovered migration omissions
have either been implemented or converted into explicit, evidence-backed Core
Gaps/hard blockers. This requirement is independent of whether the current
Provider percentages already look close to completion.
