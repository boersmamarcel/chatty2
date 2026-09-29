# Skill: analyst-panel

Description: Answer one data question by having three analysts work on it independently and, unless they agree, an adjudicator compare their derivations and pick the answer. Use it for a question about data in the workspace where one analysis is often plausibly wrong: a figure, a list, a comparison, or a finding such as "which customers churned and why". Do not use it for edits to code or files. Experimental.

## Steps

1. **Analysts.** Delegate the question to `panel-analyst-1`, then `panel-analyst-2`, then `panel-analyst-3`, one at a time. Each gets the same prompt: the question and any answer-format guidelines, word for word, and nothing else. Set `include_trace: true` on each of these three `invoke_agent` calls. Never mention one analyst's work to another. If the question says to write the answer to a file, leave that line out of the analysts' prompt, and add: "Do not write any file."
2. **Agreement.** Each analyst's result carries a `handoff` with its `answer`. Compare the three answers after normalising a copy of each: trim spaces; ignore letter case; drop a trailing `.`; read numbers as numbers (`0.50` = `0.5`, `1,000` = `1000`); compare lists item by item, ignoring spacing and a trailing comma. If all three normalised answers are equal, the panel is unanimous: skip to step 4 with analyst 1's answer exactly as it gave it. Two against one is not agreement; go to step 3.
3. **Adjudicator.** Delegate to `panel-adjudicator` with one prompt: the question and its guidelines, word for word; then, for each analyst, a heading `Candidate <n>` followed by its `answer`, its `method`, its `assumptions`, and its `trace` exactly as `invoke_agent` returned it. End the prompt with: "Which candidate's answer is most likely correct?" Its `handoff` gives `choice`, `answer` and `reason`. When `choice` is a candidate number, the panel's answer is that candidate's answer exactly as the candidate gave it; when it is `0` (a reconciled answer to an open-ended question), it is the adjudicator's `answer`.
4. **Deliver.** If the question asks for the answer to be written to a file, delegate to `panel-writer`: "Write this text to <path>, verbatim: <answer>". Then reply with the answer; how it was reached (unanimous, or the adjudicator's choice and reason); and the three analysts' answers.

## When a step fails

- An analyst whose delegation fails (an error, a timeout, an invalid handoff) drops out. Do not re-delegate to it, and do not rephrase the question for another try. Go on with the analysts that answered, numbering the candidates you pass to the adjudicator 1, 2, ... in order.
- If only one analyst answered, its answer is the panel's answer: go to step 4. If none did, reply that the panel found no answer, and why; do not answer yourself.
- If the adjudicator fails, or its `choice` names no candidate you gave it, take the answer most analysts gave (normalised as in step 2); on a tie, the first of them.
- If the writer fails, delegate to it once more with the same prompt.

## Rules

- You compute nothing and read no data yourself. Every prompt you send is self-contained.
- Never edit an analyst's answer: normalising is only for the comparison in step 2; the answer you deliver is the one the analyst or the adjudicator gave, character for character.
- Never paraphrase a trace or a method; pass them on as returned.
