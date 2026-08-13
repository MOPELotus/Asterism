use asterism_domain::QuestionKind;

/// Maps donor-audited UAI task labels whose answer shape currently has a
/// lossless representation in the shared Question/NormalizedAnswer model.
///
/// Every mapping here retains its child semantics instead of flattening all
/// donor types into a generic choice or text answer.
pub(crate) const fn audited_question_kind(task_type: &str) -> Option<QuestionKind> {
    match task_type.as_bytes() {
        b"single-choice" => Some(QuestionKind::SingleChoice),
        b"multichoice" => Some(QuestionKind::MultipleChoice),
        b"short_answer" | b"translation" | b"revise-mistake" => Some(QuestionKind::ShortAnswer),
        b"material-banked-cloze"
        | b"basic-scoop-content-dropdown"
        | b"fillblank-scoop-dropdown" => Some(QuestionKind::FillBlank),
        b"basic-scoop-content" => Some(QuestionKind::Matching),
        b"sequence" => Some(QuestionKind::Ordering),
        _ => None,
    }
}

pub(crate) const fn supports_audited_question_type(task_type: &str) -> bool {
    audited_question_kind(task_type).is_some()
}

pub(crate) fn question_kind_matches_task_type(kind: QuestionKind, task_type: &str) -> bool {
    match audited_question_kind(task_type) {
        Some(expected) if kind == expected => true,
        Some(QuestionKind::SingleChoice | QuestionKind::MultipleChoice)
            if kind == QuestionKind::Composite =>
        {
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_only_losslessly_represented_donor_types() {
        assert_eq!(
            audited_question_kind("material-banked-cloze"),
            Some(QuestionKind::FillBlank)
        );
        assert_eq!(
            audited_question_kind("translation"),
            Some(QuestionKind::ShortAnswer)
        );
        assert_eq!(
            audited_question_kind("sequence"),
            Some(QuestionKind::Ordering)
        );
        assert_eq!(
            audited_question_kind("basic-scoop-content"),
            Some(QuestionKind::Matching)
        );
        assert!(question_kind_matches_task_type(
            QuestionKind::Composite,
            "multichoice"
        ));
        assert!(!question_kind_matches_task_type(
            QuestionKind::Composite,
            "short_answer"
        ));
    }
}
