//! `best_of` (AGE-853): one task to several solvers at once, one attempt kept.
//!
//! A coordination team does not buy accuracy on sequential work; independent
//! attempts plus a selector do (`docs/research/best-of-3-prereg.md`). This
//! tool is the fan-out and the selection, done by the runtime rather than by
//! a leader model following a playbook, so neither step can be skipped,
//! paraphrased or rewritten:
//!
//! 1. **Attempts.** The task goes, word for word, to every solver of the
//!    spec's `swarm.best_of`, all at once, each through the leader's own
//!    `invoke_agent` with its trace. Each solver is its own worker process
//!    with its own context: none sees another's work. They run as many at a
//!    time as the broker's endpoint budget lets them.
//! 2. **Verifier first.** When a solver's answer carries the runner's
//!    `evidence` block with a verification line (the team declares a
//!    verification command and isolates its workers), the attempts whose
//!    command exited 0 are the only candidates. One passing attempt is
//!    the answer outright.
//! 3. **Judge.** Otherwise — or between several passing attempts — the judge
//!    gets the task and every candidate's final answer, full reply and
//!    trace, and names one by number. It never writes an answer: the
//!    answer returned is the chosen solver's, character for character.
//!    Candidates whose final answers agree need no judge.
//! 4. **Cost.** The output says what the run cost against one attempt:
//!    "3 attempts + judge, N tokens (≈3.4× a single run)".

use std::sync::Arc;

use futures::future::join_all;
use rig_agent::tool::{Tool, ToolContext};
use serde::{Deserialize, Serialize};

use crate::agent_spec::BestOfSection;
use crate::models::token_usage::TokenUsage;
use crate::tools::invoke_agent_tool::{InvokeAgentArgs, InvokeAgentOutput, InvokeAgentTool};

/// The marker a solver puts before its final answer. Everything after the
/// last one is the attempt's answer; a reply without one is its own answer.
pub const FINAL_ANSWER_MARKER: &str = "FINAL ANSWER:";

#[derive(Debug, Deserialize, Serialize)]
pub struct BestOfArgs {
    /// The task, exactly as the user gave it.
    pub task: String,
}

/// How the kept attempt was chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectedBy {
    /// The only attempt whose verification command passed.
    Verifier,
    /// The judge's choice.
    Judge,
    /// Every candidate gave the same final answer.
    Unanimous,
    /// Only one attempt finished.
    OnlyAttempt,
    /// The judge failed or named no candidate: the most common final answer,
    /// the first of them on a tie.
    Vote,
}

/// One solver's attempt, as the judge and the user see it.
#[derive(Debug, Clone, Serialize)]
pub struct Attempt {
    /// 1-based, in the spec's solver order.
    pub number: usize,
    pub agent: String,
    pub success: bool,
    /// The final answer (after the last `FINAL ANSWER:`), or the error.
    pub answer: String,
    /// `Some(true)` when the runner's verification passed, `Some(false)`
    /// when it ran and failed, `None` when none ran.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verified: Option<bool>,
    /// Input plus output tokens the attempt reported.
    pub tokens: u64,
    #[serde(skip)]
    pub response: String,
    #[serde(skip)]
    pub trace: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct BestOfOutput {
    /// The kept attempt's final answer, exactly as its solver gave it.
    pub answer: String,
    /// Which attempt it is (1-based).
    pub chosen: usize,
    pub selected_by: SelectedBy,
    /// The judge's reason, when it decided.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The kept attempt's full reply.
    pub response: String,
    pub attempts: Vec<Attempt>,
    /// "3 attempts + judge, N tokens (≈X× a single run)".
    pub cost: String,
}

#[derive(Debug, thiserror::Error)]
pub enum BestOfError {
    #[error("best_of: every attempt failed: {0}")]
    AllFailed(String),
}

/// The `best_of` tool: the spec's solvers and judge over its `invoke_agent`.
#[derive(Clone)]
pub struct BestOfTool {
    invoke: Arc<InvokeAgentTool>,
    config: BestOfSection,
}

impl BestOfTool {
    pub fn new(invoke: InvokeAgentTool, config: BestOfSection) -> Self {
        Self {
            invoke: Arc::new(invoke),
            config,
        }
    }

    async fn delegate(&self, agent: &str, prompt: String) -> Result<InvokeAgentOutput, String> {
        self.invoke
            .call(
                &mut ToolContext::new(),
                InvokeAgentArgs {
                    agent: agent.to_string(),
                    prompt,
                    include_trace: true,
                },
            )
            .await
            .map_err(|e| e.to_string())
    }
}

impl Tool for BestOfTool {
    const NAME: &'static str = "best_of";
    type Error = BestOfError;
    type Args = BestOfArgs;
    type Output = BestOfOutput;

    fn description(&self) -> String {
        format!(
            "Run the task as {} independent attempts at once ({}), then keep one: the attempt \
             that passes the team's verification, else the one the judge ({}) picks after \
             reading every attempt's answer and trace. Returns the kept attempt's answer \
             verbatim, every attempt's answer, and what the run cost. Pass the user's task \
             word for word.",
            self.config.solvers.len(),
            self.config.solvers.join(", "),
            self.config.judge
        )
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "The user's task, word for word."
                }
            },
            "required": ["task"]
        })
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let runs = join_all(
            self.config
                .solvers
                .iter()
                .map(|solver| self.delegate(solver, args.task.clone())),
        )
        .await;
        let attempts: Vec<Attempt> = self
            .config
            .solvers
            .iter()
            .zip(runs)
            .enumerate()
            .map(|(i, (agent, run))| attempt(i + 1, agent, run))
            .collect();

        let plan = plan(&attempts);
        let (chosen, selected_by, reason, judge_tokens) = match plan {
            Plan::None => {
                let errors: Vec<String> = attempts
                    .iter()
                    .map(|a| format!("{}: {}", a.agent, a.answer))
                    .collect();
                return Err(BestOfError::AllFailed(errors.join("; ")));
            }
            Plan::Decided(n, by) => (n, by, None, None),
            Plan::Judge(candidates) => {
                let prompt = judge_prompt(&args.task, &attempts, &candidates);
                match self.delegate(&self.config.judge, prompt).await {
                    Ok(out) => {
                        let tokens = tokens_of(&out.usage);
                        match judge_choice(&out, candidates.len()) {
                            Some((pick, reason)) => (
                                candidates[pick - 1],
                                SelectedBy::Judge,
                                reason,
                                Some(tokens),
                            ),
                            None => (
                                vote(&attempts, &candidates),
                                SelectedBy::Vote,
                                None,
                                Some(tokens),
                            ),
                        }
                    }
                    Err(_) => (
                        vote(&attempts, &candidates),
                        SelectedBy::Vote,
                        None,
                        Some(0),
                    ),
                }
            }
        };

        let attempt_tokens: Vec<u64> = attempts.iter().map(|a| a.tokens).collect();
        let kept = &attempts[chosen - 1];
        Ok(BestOfOutput {
            answer: kept.answer.clone(),
            chosen,
            selected_by,
            reason,
            response: kept.response.clone(),
            cost: cost_banner(&attempt_tokens, judge_tokens),
            attempts,
        })
    }
}

fn attempt(number: usize, agent: &str, run: Result<InvokeAgentOutput, String>) -> Attempt {
    match run {
        // A worker that ended without a word gets `invoke_agent`'s stand-in
        // reply; that is no attempt at an answer.
        Ok(out) if out.response == format!("Agent '{agent}' completed successfully.") => Attempt {
            number,
            agent: agent.to_string(),
            success: false,
            answer: "the attempt ended without an answer".to_string(),
            verified: None,
            tokens: tokens_of(&out.usage),
            response: String::new(),
            trace: out.trace,
        },
        Ok(out) => Attempt {
            number,
            agent: agent.to_string(),
            success: out.success,
            answer: final_answer(&out.response),
            verified: verification_passed(&out.response),
            tokens: tokens_of(&out.usage),
            response: out.response,
            trace: out.trace,
        },
        Err(error) => Attempt {
            number,
            agent: agent.to_string(),
            success: false,
            answer: error,
            verified: None,
            tokens: 0,
            response: String::new(),
            trace: None,
        },
    }
}

/// Input plus output tokens over a callee's usage lines.
fn tokens_of(usage: &[TokenUsage]) -> u64 {
    usage
        .iter()
        .map(|line| u64::from(line.input_tokens) + u64::from(line.output_tokens))
        .sum()
}

/// What a reply answers: the text after its last [`FINAL_ANSWER_MARKER`],
/// up to an `evidence` block, trimmed; the whole reply without a marker.
pub fn final_answer(response: &str) -> String {
    let body = response.split("```evidence").next().unwrap_or(response);
    match body.rfind(FINAL_ANSWER_MARKER) {
        Some(at) => body[at + FINAL_ANSWER_MARKER.len()..].trim().to_string(),
        None => body.trim().to_string(),
    }
}

/// Whether the runner's verification passed, read off the last `evidence`
/// block of the reply (`verification: <command> — exit code 0`). `None` when
/// the reply carries no verification line.
pub fn verification_passed(response: &str) -> Option<bool> {
    let block = response.rsplit("```evidence").next()?;
    if block.len() == response.len() {
        return None;
    }
    let line = block
        .lines()
        .take_while(|line| line.trim() != "```")
        .find(|line| line.starts_with("verification: "))?;
    Some(line.trim_end().ends_with("— exit code 0"))
}

/// What to do once the attempts are in.
#[derive(Debug, PartialEq)]
enum Plan {
    /// No attempt finished.
    None,
    /// Kept without a judge.
    Decided(usize, SelectedBy),
    /// The judge picks among these attempt numbers.
    Judge(Vec<usize>),
}

fn plan(attempts: &[Attempt]) -> Plan {
    let finished: Vec<usize> = attempts
        .iter()
        .filter(|a| a.success)
        .map(|a| a.number)
        .collect();
    if finished.is_empty() {
        return Plan::None;
    }
    let verified: Vec<usize> = finished
        .iter()
        .copied()
        .filter(|n| attempts[n - 1].verified == Some(true))
        .collect();
    let candidates = match verified.len() {
        1 => return Plan::Decided(verified[0], SelectedBy::Verifier),
        0 => finished,
        _ => verified,
    };
    if candidates.len() == 1 {
        return Plan::Decided(candidates[0], SelectedBy::OnlyAttempt);
    }
    let first = normalise(&attempts[candidates[0] - 1].answer);
    if candidates
        .iter()
        .all(|n| normalise(&attempts[n - 1].answer) == first)
    {
        return Plan::Decided(candidates[0], SelectedBy::Unanimous);
    }
    Plan::Judge(candidates)
}

/// For comparing answers only: case, spacing, a trailing `.` and thousands
/// separators in numbers do not count. Never what is returned.
fn normalise(answer: &str) -> String {
    let collapsed = answer.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed.trim_end_matches('.').to_lowercase();
    match trimmed.replace(',', "").parse::<f64>() {
        Ok(number) => number.to_string(),
        Err(_) => trimmed,
    }
}

/// The most common final answer among `candidates`; the first on a tie.
fn vote(attempts: &[Attempt], candidates: &[usize]) -> usize {
    let mut best = candidates[0];
    let mut best_count = 0;
    for &n in candidates {
        let key = normalise(&attempts[n - 1].answer);
        let count = candidates
            .iter()
            .filter(|&&m| normalise(&attempts[m - 1].answer) == key)
            .count();
        if count > best_count {
            best = n;
            best_count = count;
        }
    }
    best
}

/// The judge's task: the user's task, then each candidate (renumbered 1..)
/// with its final answer, its full reply and its trace.
fn judge_prompt(task: &str, attempts: &[Attempt], candidates: &[usize]) -> String {
    let mut prompt = format!("Task:\n{task}\n");
    for (i, &n) in candidates.iter().enumerate() {
        let a = &attempts[n - 1];
        prompt.push_str(&format!(
            "\n## Candidate {}\nFinal answer: {}\n\nReply:\n{}\n\nTrace:\n{}\n",
            i + 1,
            a.answer,
            a.response,
            a.trace.as_deref().unwrap_or("(none reported)")
        ));
    }
    prompt.push_str(&format!(
        "\nWhich candidate's answer is most likely correct? Answer with JSON only: \
         {{\"choice\": <1-{}>, \"reason\": \"<one or two sentences>\"}}",
        candidates.len()
    ));
    prompt
}

/// The candidate the judge named (1-based) and its reason, from its typed
/// handoff when the team names a schema, else from the first JSON object in
/// its reply. `None` when it named no candidate in range.
fn judge_choice(out: &InvokeAgentOutput, candidates: usize) -> Option<(usize, Option<String>)> {
    let value = out
        .handoff
        .clone()
        .or_else(|| first_json_object(&out.response))?;
    let choice = usize::try_from(value.get("choice")?.as_u64()?).ok()?;
    if !(1..=candidates).contains(&choice) {
        return None;
    }
    let reason = value
        .get("reason")
        .and_then(|r| r.as_str())
        .map(str::to_string);
    Some((choice, reason))
}

fn first_json_object(text: &str) -> Option<serde_json::Value> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    serde_json::from_str(text.get(start..=end)?).ok()
}

/// "3 attempts + judge, 41,200 tokens (≈3.4× a single run)". A single run is
/// the mean of the attempts that reported tokens; without any, the multiple
/// is left out rather than guessed.
pub fn cost_banner(attempt_tokens: &[u64], judge_tokens: Option<u64>) -> String {
    let n = attempt_tokens.len();
    let head = match judge_tokens {
        Some(_) => format!("{n} attempts + judge"),
        None => format!("{n} attempts, no judge needed"),
    };
    let total: u64 = attempt_tokens.iter().sum::<u64>() + judge_tokens.unwrap_or(0);
    let reported: Vec<u64> = attempt_tokens.iter().copied().filter(|t| *t > 0).collect();
    if reported.is_empty() {
        return format!("{head}, token use not reported");
    }
    let single = reported.iter().sum::<u64>() as f64 / reported.len() as f64;
    format!(
        "{head}, {} tokens (≈{:.1}× a single run)",
        thousands(total),
        total as f64 / single
    )
}

fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn att(number: usize, answer: &str, verified: Option<bool>) -> Attempt {
        Attempt {
            number,
            agent: format!("s{number}"),
            success: true,
            answer: answer.to_string(),
            verified,
            tokens: 100,
            response: answer.to_string(),
            trace: None,
        }
    }

    #[test]
    fn cost_banner_shows_multiple() {
        assert_eq!(
            cost_banner(&[10_000, 12_000, 14_000], Some(6_000)),
            "3 attempts + judge, 42,000 tokens (≈3.5× a single run)"
        );
        assert_eq!(
            cost_banner(&[10_000, 10_000, 10_000], None),
            "3 attempts, no judge needed, 30,000 tokens (≈3.0× a single run)"
        );
        assert_eq!(
            cost_banner(&[0, 0, 0], Some(0)),
            "3 attempts + judge, token use not reported"
        );
    }

    #[test]
    fn the_final_answer_is_after_the_last_marker_and_before_the_evidence() {
        let reply =
            "Work.\nFINAL ANSWER: draft\nMore.\nFINAL ANSWER:  42 \n\n```evidence\nbranch: b\n```";
        assert_eq!(final_answer(reply), "42");
        assert_eq!(final_answer("  just this  "), "just this");
    }

    #[test]
    fn the_verification_line_decides_pass_or_fail() {
        let pass = "x\n\n```evidence\nbranch: b\nverification: cargo test — exit code 0\nok\n```";
        let fail = "x\n\n```evidence\nbranch: b\nverification: cargo test — exit code 101\n```";
        let none = "x\n\n```evidence\nbranch: b\n```";
        assert_eq!(verification_passed(pass), Some(true));
        assert_eq!(verification_passed(fail), Some(false));
        assert_eq!(verification_passed(none), None);
        assert_eq!(verification_passed("no block"), None);
    }

    #[test]
    fn one_passing_attempt_is_kept_without_a_judge() {
        let attempts = [
            att(1, "a", Some(false)),
            att(2, "b", Some(true)),
            att(3, "c", Some(false)),
        ];
        assert_eq!(plan(&attempts), Plan::Decided(2, SelectedBy::Verifier));
        let two = [
            att(1, "a", Some(true)),
            att(2, "b", Some(false)),
            att(3, "c", Some(true)),
        ];
        assert_eq!(plan(&two), Plan::Judge(vec![1, 3]));
    }

    #[test]
    fn agreeing_answers_need_no_judge_and_a_split_does() {
        let same = [
            att(1, "1,000", None),
            att(2, "1000.", None),
            att(3, "1000", None),
        ];
        assert_eq!(plan(&same), Plan::Decided(1, SelectedBy::Unanimous));
        let split = [att(1, "1", None), att(2, "2", None), att(3, "2", None)];
        assert_eq!(plan(&split), Plan::Judge(vec![1, 2, 3]));
        assert_eq!(vote(&split, &[1, 2, 3]), 2);
    }
}
