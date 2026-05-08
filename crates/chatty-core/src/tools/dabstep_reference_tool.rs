use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};

use crate::services::filesystem_service::FileSystemService;
use crate::tools::ToolError;

const DABSTEP_FILES: &[&str] = &[
    "data/payments.csv",
    "data/fees.json",
    "data/merchant_data.json",
    "data/payments-readme.md",
];

#[derive(Debug, Deserialize, Serialize)]
pub struct DABStepReferenceArgs {
    /// Optional topic. Defaults to "overview".
    pub topic: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DABStepReferenceOutput {
    pub dataset: String,
    pub topic: String,
    pub summary: String,
    pub bullets: Vec<String>,
    pub recommended_files: Vec<String>,
    pub example_queries: Vec<String>,
    pub note: Option<String>,
}

#[derive(Clone, Default)]
pub struct DABStepReferenceTool;

impl DABStepReferenceTool {
    pub fn new() -> Self {
        Self
    }

    pub async fn is_available(service: &FileSystemService) -> bool {
        for path in DABSTEP_FILES {
            let Ok(resolved) = service.resolve_path(path).await else {
                return false;
            };
            let Ok(metadata) = tokio::fs::metadata(resolved).await else {
                return false;
            };
            if !metadata.is_file() {
                return false;
            }
        }

        true
    }

    fn parse_topic(raw: Option<String>) -> Result<&'static str, ToolError> {
        match raw
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("overview")
            .to_ascii_lowercase()
            .as_str()
        {
            "overview" => Ok("overview"),
            "files" | "datasets" => Ok("files"),
            "payments_schema" | "payments" | "schema" => Ok("payments_schema"),
            "merchant_profile" | "merchant" => Ok("merchant_profile"),
            "fee_rules" | "fees" | "rules" => Ok("fee_rules"),
            "month_map" | "months" | "calendar" => Ok("month_map"),
            "task_checklist" | "workflow" | "closure" => Ok("task_checklist"),
            other => Err(ToolError::OperationFailed(format!(
                "Unknown DABStep topic '{other}'. Use one of: overview, files, payments_schema, merchant_profile, fee_rules, month_map, task_checklist."
            ))),
        }
    }

    fn overview() -> DABStepReferenceOutput {
        DABStepReferenceOutput {
            dataset: "adyen/dabstep".to_string(),
            topic: "overview".to_string(),
            summary: "Use merchant_data.json for the merchant profile, payments.csv for the target slice, fees.json for candidate rule matching, and only open manual.md when a rule detail is still ambiguous.".to_string(),
            bullets: vec![
                "Recommended order: merchant profile -> target month/year slice -> monthly volume / fraud slice -> candidate fee rules -> final comparison -> write /app/answer.txt.".to_string(),
                "Prefer aggregate SQL first on payments.csv: COUNT, SUM(eur_amount), GROUP BY, MIN/MAX, and narrow previews with LIMIT 5.".to_string(),
                "For DABStep paths, use data/<file> or /app/data/<file>; avoid bare file names like payments.csv.".to_string(),
                "If execute_code is available, use one compact stdlib Python script for repetitive fee-rule filtering instead of many exploratory tool calls.".to_string(),
            ],
            recommended_files: vec![
                "data/payments-readme.md".to_string(),
                "data/merchant_data.json".to_string(),
                "data/fees.json".to_string(),
                "data/manual.md".to_string(),
            ],
            example_queries: vec![
                "SELECT SUM(eur_amount) AS total_volume, COUNT(*) AS total_count FROM 'data/payments.csv' WHERE merchant = '...' AND day_of_year BETWEEN 91 AND 120".to_string(),
                "SELECT aci, COUNT(*) AS count, SUM(eur_amount) AS volume FROM 'data/payments.csv' WHERE merchant = '...' AND day_of_year BETWEEN 91 AND 120 GROUP BY aci".to_string(),
            ],
            note: Some(
                "Use a more specific topic before reading the full manual: files, payments_schema, merchant_profile, fee_rules, month_map, or task_checklist."
                    .to_string(),
            ),
        }
    }

    fn files() -> DABStepReferenceOutput {
        DABStepReferenceOutput {
            dataset: "adyen/dabstep".to_string(),
            topic: "files".to_string(),
            summary: "The benchmark revolves around one large transaction table plus a small merchant profile file, a fee-rule table, and two lookup/reference documents.".to_string(),
            bullets: vec![
                "data/payments.csv: main transaction table; use SQL aggregates for counts, volume, fraud slices, ACI breakdowns, country filters, and candidate plan comparisons.".to_string(),
                "data/payments-readme.md: compact payments.csv schema reference; read this before touching manual.md.".to_string(),
                "data/merchant_data.json: merchant-level metadata such as merchant name, capture_delay, acquirer list, merchant_category_code, and account_type.".to_string(),
                "data/fees.json: candidate fee rules keyed by merchant/payment attributes and monthly buckets.".to_string(),
                "data/manual.md: long merchant guide; only open it when the compact summaries here are not enough.".to_string(),
                "data/merchant_category_codes.csv and data/acquirer_countries.csv: lookup tables when a task needs MCC or country interpretation.".to_string(),
            ],
            recommended_files: vec![
                "data/payments-readme.md".to_string(),
                "data/merchant_data.json".to_string(),
                "data/fees.json".to_string(),
            ],
            example_queries: vec![
                "SELECT COUNT(*), SUM(eur_amount) FROM 'data/payments.csv' WHERE merchant = '...'".to_string(),
            ],
            note: None,
        }
    }

    fn payments_schema() -> DABStepReferenceOutput {
        DABStepReferenceOutput {
            dataset: "adyen/dabstep".to_string(),
            topic: "payments_schema".to_string(),
            summary: "payments.csv is the transaction fact table. Most hard tasks only need a small subset of its columns plus one or two aggregates.".to_string(),
            bullets: vec![
                "Core identifiers: psp_reference, merchant, ip_address, email_address, card_number, card_bin.".to_string(),
                "Timing: year, hour_of_day, minute_of_hour, day_of_year.".to_string(),
                "Payment attributes: card_scheme, is_credit, shopper_interaction, aci, acquirer_country.".to_string(),
                "Amount and geography: eur_amount, ip_country, issuing_country.".to_string(),
                "Risk outcomes: has_fraudulent_dispute, is_refused_by_adyen.".to_string(),
                "Typical fee tasks filter by merchant + period, then aggregate volume/count and sometimes fraud or ACI slices before comparing fee rules.".to_string(),
            ],
            recommended_files: vec!["data/payments-readme.md".to_string()],
            example_queries: vec![
                "SELECT merchant, COUNT(*), SUM(eur_amount) FROM 'data/payments.csv' GROUP BY merchant ORDER BY SUM(eur_amount) DESC LIMIT 5".to_string(),
                "SELECT COUNT(*) AS disputed_count, SUM(eur_amount) AS disputed_volume FROM 'data/payments.csv' WHERE merchant = '...' AND has_fraudulent_dispute = true".to_string(),
            ],
            note: None,
        }
    }

    fn merchant_profile() -> DABStepReferenceOutput {
        DABStepReferenceOutput {
            dataset: "adyen/dabstep".to_string(),
            topic: "merchant_profile".to_string(),
            summary: "merchant_data.json gives the merchant-side attributes that many fee rules depend on before you ever look at payments.csv.".to_string(),
            bullets: vec![
                "Observed merchant_data.json fields: merchant, capture_delay, acquirer, merchant_category_code, account_type.".to_string(),
                "capture_delay may appear as values like immediate, manual, or numeric strings such as 1, 2, 7.".to_string(),
                "When matching fee rules, numeric capture delays usually need to be mapped into rule buckets such as <3, 3-5, or >5.".to_string(),
                "account_type is a single merchant code (examples observed in the benchmark: R, H, F, D, S).".to_string(),
                "merchant_category_code is a single MCC on the merchant profile; fees.json may store MCC conditions as a list of allowed MCCs.".to_string(),
                "acquirer is a list of allowed acquiring banks for that merchant; use it when a task asks about acquirer choice or cross-country behavior.".to_string(),
            ],
            recommended_files: vec!["data/merchant_data.json".to_string()],
            example_queries: vec![
                "Read merchant_data.json first, then filter payments.csv by that exact merchant name.".to_string(),
            ],
            note: None,
        }
    }

    fn fee_rules() -> DABStepReferenceOutput {
        DABStepReferenceOutput {
            dataset: "adyen/dabstep".to_string(),
            topic: "fee_rules".to_string(),
            summary: "fees.json is the candidate-rule table. The hard part is not reading it, but filtering it in the right order so you stop with a small candidate set.".to_string(),
            bullets: vec![
                "Observed fees.json fields: card_scheme, account_type, capture_delay, monthly_fraud_level, monthly_volume, merchant_category_code, is_credit, aci, fixed_amount, rate, intracountry.".to_string(),
                "Some rule dimensions are scalar selectors (for example card_scheme or capture_delay); others may be lists of allowed values (for example account_type, merchant_category_code, or aci).".to_string(),
                "Rule buckets matter: capture_delay is bucketed (examples observed: immediate, manual, <3, 3-5, >5) and monthly_volume / monthly_fraud_level are also bucketed ranges.".to_string(),
                "A common matching flow is: merchant account_type + MCC + capture_delay -> payment attributes (scheme / credit / ACI / cross-border) -> monthly volume / fraud bucket -> compare fixed_amount and rate.".to_string(),
                "Do not scan the whole fee table in the model loop. Filter it deterministically in SQL or a single Python script, then inspect only the final candidate rows.".to_string(),
                "If no specific MCC rule exists for the merchant, check whether broader rules with empty or broad selectors apply before assuming the merchant is unsupported.".to_string(),
            ],
            recommended_files: vec![
                "data/fees.json".to_string(),
                "data/manual.md".to_string(),
            ],
            example_queries: vec![
                "Use execute_code or SQL to filter fees.json by merchant-derived attributes before manually inspecting rows.".to_string(),
            ],
            note: Some(
                "This summary is intentionally compact. Open manual.md only if you still need a precise interpretation for a specific rule dimension."
                    .to_string(),
            ),
        }
    }

    fn month_map() -> DABStepReferenceOutput {
        DABStepReferenceOutput {
            dataset: "adyen/dabstep".to_string(),
            topic: "month_map".to_string(),
            summary: "day_of_year is the transaction date key used in DABStep fee tasks."
                .to_string(),
            bullets: vec![
                "January: 1-31".to_string(),
                "February: 32-59".to_string(),
                "March: 60-90".to_string(),
                "April: 91-120".to_string(),
                "May: 121-151".to_string(),
                "June: 152-181".to_string(),
                "July: 182-212".to_string(),
                "August: 213-243".to_string(),
                "September: 244-273".to_string(),
                "October: 274-304".to_string(),
                "November: 305-334".to_string(),
                "December: 335-365".to_string(),
            ],
            recommended_files: vec!["data/payments.csv".to_string()],
            example_queries: vec![
                "WHERE year = 2024 AND day_of_year BETWEEN 91 AND 120".to_string(),
            ],
            note: None,
        }
    }

    fn task_checklist() -> DABStepReferenceOutput {
        DABStepReferenceOutput {
            dataset: "adyen/dabstep".to_string(),
            topic: "task_checklist".to_string(),
            summary: "Most DABStep failures come from over-exploring instead of closing. Use this as the short closure checklist.".to_string(),
            bullets: vec![
                "Read payments-readme.md or this tool before opening manual.md.".to_string(),
                "Get the merchant profile first from merchant_data.json.".to_string(),
                "Compute the target month or year slice from payments.csv with aggregates, not raw row dumps.".to_string(),
                "Derive monthly volume and fraud slices before filtering fees.json.".to_string(),
                "If two consecutive queries do not materially change the candidate answer, stop exploring and write /app/answer.txt.".to_string(),
                "Write only the required final answer, not an explanation, unless the task explicitly asks for one.".to_string(),
            ],
            recommended_files: vec![
                "data/merchant_data.json".to_string(),
                "data/payments.csv".to_string(),
                "/app/answer.txt".to_string(),
            ],
            example_queries: vec![
                "SELECT SUM(eur_amount), COUNT(*) FROM 'data/payments.csv' WHERE merchant = '...' AND year = 2024".to_string(),
            ],
            note: None,
        }
    }
}

impl Tool for DABStepReferenceTool {
    const NAME: &'static str = "dabstep_reference";
    type Error = ToolError;
    type Args = DABStepReferenceArgs;
    type Output = DABStepReferenceOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "dabstep_reference".to_string(),
            description: "Return a compact DABStep benchmark reference without reading the large manual. Use this first in DABStep workspaces to get the file map, payments schema, merchant profile fields, fee-rule dimensions, month/day-of-year mapping, or a short task checklist.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "topic": {
                        "type": "string",
                        "description": "Optional topic. One of: overview, files, payments_schema, merchant_profile, fee_rules, month_map, task_checklist. Defaults to overview."
                    }
                },
                "required": []
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        Ok(match Self::parse_topic(args.topic)? {
            "overview" => Self::overview(),
            "files" => Self::files(),
            "payments_schema" => Self::payments_schema(),
            "merchant_profile" => Self::merchant_profile(),
            "fee_rules" => Self::fee_rules(),
            "month_map" => Self::month_map(),
            "task_checklist" => Self::task_checklist(),
            _ => unreachable!("topic parser returned unexpected value"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_overview() {
        assert_eq!(DABStepReferenceTool::parse_topic(None).unwrap(), "overview");
    }

    #[test]
    fn accepts_aliases() {
        assert_eq!(
            DABStepReferenceTool::parse_topic(Some("workflow".to_string())).unwrap(),
            "task_checklist"
        );
        assert_eq!(
            DABStepReferenceTool::parse_topic(Some("rules".to_string())).unwrap(),
            "fee_rules"
        );
    }

    #[test]
    fn rejects_unknown_topics() {
        let err = DABStepReferenceTool::parse_topic(Some("mystery".to_string())).unwrap_err();
        assert!(
            err.to_string().contains("Unknown DABStep topic"),
            "unexpected error: {err}"
        );
    }
}
