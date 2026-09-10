//! The agent's todo plan as one card, instead of a log of `update_todo` calls.
//!
//! The todo tools are state mutations: rendering each call as its own event
//! costs twelve transcript rows for a three-step plan and still never shows a
//! todo's title. This module turns the latest [`AgentTaskSnapshot`] into the
//! rows of a single card that is rewritten in place as the plan advances —
//! the same model the desktop client's `PlanBlock` uses.
//!
//! Pure like [`crate::ui::tool_summary`]: rows in, no ratatui types, so the
//! layout is testable without a frame.

use chatty_core::services::{AgentTaskSnapshot, AgentTodoStatus};

/// Glyph column plus its trailing space, on every row of the card.
const GLYPH_WIDTH: usize = 2;

/// Indent of the card header, matching the tool-call glyph column.
const HEADER_INDENT: usize = 2;

/// Indent of a step row, one step in from the header.
const STEP_INDENT: usize = 5;

/// A blocked step's reason sits in the step's glyph column, so its text lines
/// up under the title above it.
const REASON_INDENT: usize = STEP_INDENT;

/// Narrowest body worth printing; below this the goal and titles are dropped
/// rather than truncated to a bare `…`.
const MIN_BODY: usize = 12;

/// What a row means, mapped to a concrete `Style` by the chat view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Header,
    Done,
    Running,
    Pending,
    Blocked,
    Subtle,
}

/// One row of the card: `<indent><glyph> <text>…<trailing>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanRow {
    pub indent: usize,
    pub glyph: &'static str,
    pub tone: Tone,
    pub text: String,
    /// Right-aligned tail — the counts on the header, `blocked` on a step.
    pub trailing: String,
}

impl PlanRow {
    /// Columns between `text` and `trailing` on a `width`-column terminal.
    pub fn padding(&self, width: usize) -> usize {
        let used =
            self.indent + GLYPH_WIDTH + self.text.chars().count() + self.trailing.chars().count();
        width.saturating_sub(used).max(1)
    }
}

/// `(done, total, blocked, current step)` — mirrors the desktop client's
/// `plan_counts` so the two frontends never disagree about progress.
pub fn plan_counts(snapshot: &AgentTaskSnapshot) -> (usize, usize, usize, String) {
    let done = count(snapshot, AgentTodoStatus::Done);
    let blocked = count(snapshot, AgentTodoStatus::Blocked);
    let total = snapshot.todos.len().max(1);
    let current = first_with(snapshot, AgentTodoStatus::InProgress)
        .or_else(|| first_with(snapshot, AgentTodoStatus::Pending))
        .unwrap_or_else(|| format!("{done}/{total} complete"));
    (done, total, blocked, current)
}

/// The card's rows for `snapshot` on a `width`-column terminal.
///
/// `collapsed` keeps only the header, for a plan that is finished or has been
/// superseded — the desktop client's `retain_last_plan_block` rule.
pub fn plan_rows(snapshot: &AgentTaskSnapshot, width: usize, collapsed: bool) -> Vec<PlanRow> {
    let (done, total, blocked, _) = plan_counts(snapshot);
    let finished = done == total && !snapshot.todos.is_empty();

    let trailing = if snapshot.verification_skipped {
        format!("{done}/{total} · verification skipped")
    } else if blocked > 0 {
        format!("{done}/{total} · {blocked} blocked")
    } else if snapshot.verified {
        format!("{done}/{total} verified")
    } else {
        format!("{done}/{total}")
    };

    let goal = snapshot.goal.as_deref().unwrap_or("").trim();
    let header_body = clip(
        &if goal.is_empty() {
            "Plan".to_string()
        } else {
            format!("Plan · {goal}")
        },
        body_budget(width, HEADER_INDENT, trailing.chars().count()),
    );

    let mut rows = vec![PlanRow {
        indent: HEADER_INDENT,
        glyph: if finished && snapshot.verified {
            "✔"
        } else {
            "▣"
        },
        tone: Tone::Header,
        text: header_body,
        trailing,
    }];

    if collapsed {
        return rows;
    }

    for todo in &snapshot.todos {
        let (glyph, tone, status) = match todo.status {
            AgentTodoStatus::Done => ("✔", Tone::Done, ""),
            AgentTodoStatus::InProgress => ("▸", Tone::Running, ""),
            AgentTodoStatus::Pending => ("○", Tone::Pending, ""),
            AgentTodoStatus::Blocked => ("✖", Tone::Blocked, "blocked"),
        };

        rows.push(PlanRow {
            indent: STEP_INDENT,
            glyph,
            tone,
            text: clip(
                todo.title.trim(),
                body_budget(width, STEP_INDENT, status.chars().count()),
            ),
            trailing: status.to_string(),
        });

        // Why a step stopped is the user's problem; `reflection` is the
        // model's note to itself and stays out of the card.
        if let Some(reason) = todo.blocked_reason.as_deref().map(str::trim)
            && matches!(todo.status, AgentTodoStatus::Blocked)
            && !reason.is_empty()
        {
            rows.push(PlanRow {
                indent: REASON_INDENT,
                glyph: " ",
                tone: Tone::Subtle,
                text: clip(reason, body_budget(width, REASON_INDENT, 0)),
                trailing: String::new(),
            });
        }
    }

    rows
}

/// Below this the status bar drops the plan segment rather than showing a
/// fragment of it.
pub const MIN_STATUS_WIDTH: usize = 18;

/// The one-line plan summary for the status bar: `●●▸  Plan 2 of 3 · <step>`.
///
/// This is what makes collapsing the card affordable — the position stays on
/// screen once the transcript has scrolled past the plan.
pub fn status_line(snapshot: &AgentTaskSnapshot, width: usize) -> String {
    let (done, total, blocked, current) = plan_counts(snapshot);
    let dots: String = snapshot
        .todos
        .iter()
        .map(|todo| match todo.status {
            AgentTodoStatus::Done => '●',
            AgentTodoStatus::InProgress => '▸',
            AgentTodoStatus::Blocked => '✖',
            AgentTodoStatus::Pending => '○',
        })
        .collect();

    let counts = if blocked > 0 {
        format!("Plan {done} of {total} · {blocked} blocked")
    } else {
        format!("Plan {done} of {total}")
    };

    let head = format!("{dots}  {counts}");
    let budget = body_budget(width, head.chars().count() + 3, 0);
    if budget < MIN_BODY {
        return clip(&head, width);
    }
    format!("{head} · {}", clip(current.trim(), budget))
}

fn count(snapshot: &AgentTaskSnapshot, status: AgentTodoStatus) -> usize {
    snapshot
        .todos
        .iter()
        .filter(|todo| todo.status == status)
        .count()
}

fn first_with(snapshot: &AgentTaskSnapshot, status: AgentTodoStatus) -> Option<String> {
    snapshot
        .todos
        .iter()
        .find(|todo| todo.status == status)
        .map(|todo| todo.title.trim().to_string())
}

/// Columns left for a row's body once its indent, glyph and tail are paid for.
fn body_budget(width: usize, indent: usize, trailing: usize) -> usize {
    width.saturating_sub(indent + GLYPH_WIDTH + trailing + 1)
}

/// Clip to `budget` columns with an ellipsis, or to nothing when the budget is
/// too small to say anything — a wrapped row would break the card's indent,
/// because ratatui restarts a wrapped line at column 0.
fn clip(text: &str, budget: usize) -> String {
    if text.chars().count() <= budget {
        return text.to_string();
    }
    if budget < MIN_BODY {
        return String::new();
    }
    let kept: String = text.chars().take(budget.saturating_sub(1)).collect();
    format!("{}…", kept.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chatty_core::services::AgentTodo;

    fn todo(title: &str, status: AgentTodoStatus) -> AgentTodo {
        AgentTodo {
            id: title.to_string(),
            title: title.to_string(),
            description: "d".to_string(),
            status,
            blocked_reason: None,
            reflection: None,
        }
    }

    fn snapshot(todos: Vec<AgentTodo>) -> AgentTaskSnapshot {
        AgentTaskSnapshot {
            goal: Some("Read and summarize all files in the workspace".to_string()),
            todos,
            write_todos_called: true,
            verified: false,
            verification_reason: None,
            evidence: Vec::new(),
            verification_skipped: false,
        }
    }

    fn three_steps() -> AgentTaskSnapshot {
        snapshot(vec![
            todo("List workspace files", AgentTodoStatus::Done),
            todo("Read all text files", AgentTodoStatus::InProgress),
            todo("Summarize findings", AgentTodoStatus::Pending),
        ])
    }

    #[test]
    fn counts_match_the_desktop_clients_reading_of_the_same_plan() {
        let (done, total, blocked, current) = plan_counts(&three_steps());

        assert_eq!((done, total, blocked), (1, 3, 0));
        assert_eq!(current, "Read all text files");
    }

    /// With nothing running, the next pending step is where we are.
    #[test]
    fn the_current_step_falls_back_to_the_first_pending_one() {
        let plan = snapshot(vec![
            todo("List workspace files", AgentTodoStatus::Done),
            todo("Read all text files", AgentTodoStatus::Pending),
        ]);

        assert_eq!(plan_counts(&plan).3, "Read all text files");
    }

    #[test]
    fn verification_skipped_is_reported_on_the_header() {
        let mut plan = three_steps();
        plan.verification_skipped = true;

        assert_eq!(
            plan_rows(&plan, 80, false)[0].trailing,
            "1/3 · verification skipped"
        );
    }

    /// A narrow terminal drops the goal rather than wrapping the header, which
    /// would restart at column 0 and break the card's indent.
    #[test]
    fn the_goal_gives_way_before_the_counts_do() {
        let plan = three_steps();

        let wide = &plan_rows(&plan, 120, false)[0];
        assert_eq!(
            wide.text,
            "Plan · Read and summarize all files in the workspace"
        );

        let narrow = &plan_rows(&plan, 40, false)[0];
        assert!(narrow.text.ends_with('…'), "{:?}", narrow.text);
        assert_eq!(narrow.trailing, "1/3");

        let tiny = &plan_rows(&plan, 16, false)[0];
        assert_eq!(tiny.text, "", "no room for a goal at 16 columns");
        assert_eq!(tiny.trailing, "1/3");
    }

    #[test]
    fn a_collapsed_plan_is_its_header() {
        assert_eq!(plan_rows(&three_steps(), 80, true).len(), 1);
    }

    #[test]
    fn the_status_line_shows_the_dots_the_counts_and_the_step() {
        assert_eq!(
            status_line(&three_steps(), 60),
            "●▸○  Plan 1 of 3 · Read all text files"
        );
    }

    #[test]
    fn the_status_line_drops_the_step_before_it_overflows() {
        let line = status_line(&three_steps(), MIN_STATUS_WIDTH);

        assert_eq!(line, "●▸○  Plan 1 of 3");
        assert!(line.chars().count() <= MIN_STATUS_WIDTH);
    }

    #[test]
    fn a_blocked_plan_says_so_everywhere() {
        let plan = snapshot(vec![
            todo("Collect merged PRs", AgentTodoStatus::Done),
            todo("Fetch the template", AgentTodoStatus::Blocked),
        ]);

        assert_eq!(plan_rows(&plan, 80, false)[0].trailing, "1/2 · 1 blocked");
        assert!(status_line(&plan, 60).contains("Plan 1 of 2 · 1 blocked"));
    }

    /// Every row has to fit, at every width the card is drawn at.
    #[test]
    fn no_row_is_wider_than_the_terminal() {
        let mut plan = three_steps();
        plan.todos.push(AgentTodo {
            blocked_reason: Some("the file turned out to be a binary blob".to_string()),
            ..todo(
                "Summarize the findings into one short paragraph",
                AgentTodoStatus::Blocked,
            )
        });

        for width in [40usize, 52, 80, 120, 200] {
            for row in plan_rows(&plan, width, false) {
                let used = row.indent
                    + GLYPH_WIDTH
                    + row.text.chars().count()
                    + row.padding(width)
                    + row.trailing.chars().count();
                assert!(used <= width, "row {row:?} needs {used} of {width} columns");
            }
            assert!(status_line(&plan, width).chars().count() <= width);
        }
    }
}
