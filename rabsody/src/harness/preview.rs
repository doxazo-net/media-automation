//! Consistent per-change preview output, shared by every write command so dry-run
//! and apply runs read identically (only the action label differs).

use super::{WriteOutcome, WriteRequest};

/// One line describing a single item's write (or would-be write):
/// `[would-apply] li_abc Some Book (metadata)`.
pub fn format_line(req: &WriteRequest, outcome: &WriteOutcome) -> String {
    let base = format!(
        "[{}] {} {} ({})",
        outcome.label(),
        req.item_id,
        req.label,
        req.operation
    );
    match outcome {
        WriteOutcome::Error(msg) => format!("{base}: {msg}"),
        WriteOutcome::Skipped(reason) => format!("{base}: {reason}"),
        _ => base,
    }
}

/// Batch tail summary: counts per outcome across a run.
pub fn format_summary(outcomes: &[WriteOutcome]) -> String {
    let mut applied = 0usize;
    let mut would = 0usize;
    let mut skipped = 0usize;
    let mut errored = 0usize;
    for o in outcomes {
        match o {
            WriteOutcome::Applied => applied += 1,
            WriteOutcome::WouldApply => would += 1,
            WriteOutcome::Skipped(_) => skipped += 1,
            WriteOutcome::Error(_) => errored += 1,
        }
    }
    format!("applied={applied} would-apply={would} skipped={skipped} error={errored}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req() -> WriteRequest {
        WriteRequest {
            server: "s".to_string(),
            item_id: "li_abc".to_string(),
            label: "Some Book".to_string(),
            operation: "metadata".to_string(),
            before: serde_json::Value::Null,
            after: serde_json::Value::Null,
        }
    }

    #[test]
    fn line_shows_action_id_label_operation() {
        assert_eq!(
            format_line(&req(), &WriteOutcome::WouldApply),
            "[would-apply] li_abc Some Book (metadata)"
        );
        assert_eq!(
            format_line(&req(), &WriteOutcome::Applied),
            "[applied] li_abc Some Book (metadata)"
        );
    }

    #[test]
    fn summary_counts_each_outcome() {
        let outcomes = vec![
            WriteOutcome::Applied,
            WriteOutcome::Applied,
            WriteOutcome::WouldApply,
            WriteOutcome::Skipped("no change".to_string()),
            WriteOutcome::Error("boom".to_string()),
        ];
        assert_eq!(
            format_summary(&outcomes),
            "applied=2 would-apply=1 skipped=1 error=1"
        );
    }
}
