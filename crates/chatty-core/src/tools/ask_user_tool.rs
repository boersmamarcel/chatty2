use rig_agent::tool::{Tool, ToolContext, ToolExecutionError};
use serde::{Deserialize, Serialize};

use crate::models::clarification_store::{
    ClarificationAnswer, ClarifyingQuestion, MAX_CLARIFYING_QUESTIONS, MAX_QUESTION_OPTIONS,
    PendingClarifications, request_clarification,
};
use crate::tools::ToolError;

#[derive(Debug, Deserialize, Serialize)]
pub struct QuestionInput {
    pub id: String,
    pub question: String,
    pub options: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct AskUserArgs {
    pub questions: Vec<QuestionInput>,
}

#[derive(Debug, Serialize)]
pub struct AskUserResponse {
    pub answers: Vec<ClarificationAnswer>,
}

/// Lets the agent pause mid-turn and ask the user to disambiguate.
///
/// The call blocks on the user's answer (see
/// [`request_clarification`]), so the live stream stays open and the model
/// continues in the same turn once the answers come back.
#[derive(Clone)]
pub struct AskUserTool {
    pending_clarifications: PendingClarifications,
}

impl AskUserTool {
    pub fn new(pending_clarifications: PendingClarifications) -> Self {
        Self {
            pending_clarifications,
        }
    }
}

impl Tool for AskUserTool {
    const NAME: &'static str = "ask_user";
    type Error = ToolError;
    type Args = AskUserArgs;
    type Output = AskUserResponse;

    fn description(&self) -> String {
        format!(
            "Ask the user up to {MAX_CLARIFYING_QUESTIONS} clarifying questions when the request is \
             genuinely ambiguous and different readings would lead to materially different work. \
             Each question offers pre-made options; the user can always type their own answer \
             instead, so you do not need to add an \"other\" option yourself. Prefer making a \
             sensible assumption and saying so over asking about something with an obvious default. \
             Do not use this to ask for permission to act, to confirm work you have already done, \
             or to report progress."
        )
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "questions": {
                    "type": "array",
                    "description": format!(
                        "The questions to put to the user, at most {MAX_CLARIFYING_QUESTIONS}."
                    ),
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": {
                                "type": "string",
                                "description": "Stable short id used to match the answer back, such as q1 or database-choice."
                            },
                            "question": {
                                "type": "string",
                                "description": "The question, phrased so it can be answered without scrolling back through the conversation."
                            },
                            "options": {
                                "type": "array",
                                "description": format!(
                                    "Between 2 and {MAX_QUESTION_OPTIONS} concrete, mutually \
                                     exclusive answers. Put the option you would recommend first."
                                ),
                                "items": { "type": "string" }
                            }
                        },
                        "required": ["id", "question", "options"]
                    }
                }
            },
            "required": ["questions"]
        })
    }

    /// Keep the real failure text in front of the user and the model:
    /// rig's default `map_error` redacts it to "the tool failed" (AGE-187).
    fn map_error(&self, error: Self::Error) -> ToolExecutionError {
        crate::tools::map_tool_error(Self::NAME, error)
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let questions = validate_questions(args.questions)?;

        let answers = request_clarification(&self.pending_clarifications, questions)
            .await
            .map_err(|e| ToolError::OperationFailed(e.to_string()))?;

        Ok(AskUserResponse { answers })
    }
}

/// Reject question sets the popover cannot render sensibly, with a message the
/// model can act on rather than a silent truncation.
fn validate_questions(questions: Vec<QuestionInput>) -> Result<Vec<ClarifyingQuestion>, ToolError> {
    if questions.is_empty() {
        return Err(ToolError::OperationFailed(
            "ask_user needs at least one question".to_string(),
        ));
    }
    if questions.len() > MAX_CLARIFYING_QUESTIONS {
        return Err(ToolError::OperationFailed(format!(
            "ask_user accepts at most {MAX_CLARIFYING_QUESTIONS} questions, got {}. Ask the most \
             important ones now and follow up if you still need more.",
            questions.len()
        )));
    }

    let mut seen_ids = std::collections::HashSet::new();
    let mut validated = Vec::with_capacity(questions.len());

    for q in questions {
        if q.question.trim().is_empty() {
            return Err(ToolError::OperationFailed(format!(
                "question '{}' has empty text",
                q.id
            )));
        }
        if !seen_ids.insert(q.id.clone()) {
            return Err(ToolError::OperationFailed(format!(
                "duplicate question id '{}' — ids must be unique so answers can be matched back",
                q.id
            )));
        }

        // Blank options would render as unclickable empty buttons.
        let options: Vec<String> = q
            .options
            .into_iter()
            .filter(|o| !o.trim().is_empty())
            .collect();

        if options.len() < 2 {
            return Err(ToolError::OperationFailed(format!(
                "question '{}' needs at least 2 options; the user can always type a custom answer, \
                 so an \"other\" option is not needed",
                q.id
            )));
        }
        if options.len() > MAX_QUESTION_OPTIONS {
            return Err(ToolError::OperationFailed(format!(
                "question '{}' has {} options, at most {MAX_QUESTION_OPTIONS} are allowed",
                q.id,
                options.len()
            )));
        }

        validated.push(ClarifyingQuestion {
            id: q.id,
            question: q.question,
            options,
        });
    }

    Ok(validated)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(id: &str, options: &[&str]) -> QuestionInput {
        QuestionInput {
            id: id.to_string(),
            question: "Which database?".to_string(),
            options: options.iter().map(|o| o.to_string()).collect(),
        }
    }

    #[test]
    fn accepts_a_well_formed_question() {
        let out = validate_questions(vec![input("q1", &["Postgres", "SQLite"])]).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].options.len(), 2);
    }

    #[test]
    fn rejects_empty_question_set() {
        assert!(validate_questions(vec![]).is_err());
    }

    #[test]
    fn rejects_too_many_questions() {
        let many = (0..=MAX_CLARIFYING_QUESTIONS)
            .map(|i| input(&format!("q{i}"), &["a", "b"]))
            .collect();
        assert!(validate_questions(many).is_err());
    }

    #[test]
    fn rejects_duplicate_ids() {
        let dupes = vec![input("q1", &["a", "b"]), input("q1", &["c", "d"])];
        let err = validate_questions(dupes).unwrap_err();
        assert!(err.to_string().contains("duplicate"), "got: {err}");
    }

    /// A single option is not a choice, and blank strings render as dead buttons.
    #[test]
    fn rejects_fewer_than_two_usable_options() {
        assert!(validate_questions(vec![input("q1", &["only"])]).is_err());
        assert!(validate_questions(vec![input("q1", &["real", "   "])]).is_err());
    }

    #[test]
    fn rejects_too_many_options() {
        let opts: Vec<String> = (0..=MAX_QUESTION_OPTIONS)
            .map(|i| format!("o{i}"))
            .collect();
        let refs: Vec<&str> = opts.iter().map(|s| s.as_str()).collect();
        assert!(validate_questions(vec![input("q1", &refs)]).is_err());
    }
}
