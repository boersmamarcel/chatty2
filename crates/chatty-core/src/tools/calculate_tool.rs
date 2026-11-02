//! `calculate`: exact decimal arithmetic and basic statistics (AGE-942).
//!
//! A reviewer is told to recompute every figure with a tool rather than in
//! its head, and an analyst to compute every number. `query_data` (DuckDB)
//! served as that calculator, but a hosted worker is built without DuckDB, so
//! it had none. This tool is in every build: a small parser over
//! `rust_decimal` (28 significant digits, so `0.1 + 0.2` is `0.3`), no I/O,
//! no variables, no loops, and bounded input, so its cost is linear in an
//! expression of at most [`MAX_EXPRESSION_BYTES`].

use rig_agent::tool::{Tool, ToolContext, ToolExecutionError};
use rust_decimal::prelude::*;
use rust_decimal::{MathematicalOps, RoundingStrategy};
use serde::{Deserialize, Serialize};

/// The longest expression accepted, in bytes.
pub const MAX_EXPRESSION_BYTES: usize = 4096;

/// How deeply parentheses, function calls, signs and powers may nest.
const MAX_DEPTH: usize = 64;

/// The most decimal places `round` takes: all `rust_decimal` can hold.
const MAX_PLACES: u32 = 28;

#[derive(Debug, Deserialize, Serialize)]
pub struct CalculateArgs {
    pub expression: String,
}

#[derive(Debug, Serialize)]
pub struct CalculateOutput {
    pub expression: String,
    pub result: String,
}

/// Why an expression has no value. Parse errors carry the 1-based character
/// column they were found at.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CalculateError {
    #[error("the expression is empty")]
    Empty,
    #[error("the expression is {0} bytes; the limit is {MAX_EXPRESSION_BYTES}")]
    TooLong(usize),
    #[error("nested more than {MAX_DEPTH} levels deep")]
    TooDeep,
    #[error("at column {column}: {message}")]
    Parse { column: usize, message: String },
    #[error("division by zero")]
    DivisionByZero,
    #[error("{0}")]
    Domain(String),
    #[error("the result is too large or too precise for 28 significant digits ({0})")]
    Overflow(String),
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Number(Decimal),
    Ident(String),
    Op(char),
    LParen,
    RParen,
    Comma,
}

/// Split the expression into tokens, each with its 1-based column.
fn tokenize(input: &str) -> Result<Vec<(Token, usize)>, CalculateError> {
    let chars: Vec<char> = input.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let column = i + 1;
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c.is_ascii_digit() || c == '.' {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            // An exponent: `e`/`E`, an optional sign, then digits.
            if i < chars.len() && matches!(chars[i], 'e' | 'E') {
                let mut j = i + 1;
                if j < chars.len() && matches!(chars[j], '+' | '-') {
                    j += 1;
                }
                if j < chars.len() && chars[j].is_ascii_digit() {
                    while j < chars.len() && chars[j].is_ascii_digit() {
                        j += 1;
                    }
                    i = j;
                }
            }
            let text: String = chars[start..i].iter().collect();
            tokens.push((Token::Number(parse_number(&text, column)?), column));
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let name: String = chars[start..i].iter().collect();
            tokens.push((Token::Ident(name.to_ascii_lowercase()), column));
            continue;
        }
        let token = match c {
            '*' if chars.get(i + 1) == Some(&'*') => {
                i += 1;
                Token::Op('^')
            }
            '+' | '-' | '*' | '/' | '%' | '^' => Token::Op(c),
            '(' => Token::LParen,
            ')' => Token::RParen,
            ',' => Token::Comma,
            _ => {
                return Err(CalculateError::Parse {
                    column,
                    message: format!("unexpected character '{c}'"),
                });
            }
        };
        tokens.push((token, column));
        i += 1;
    }
    Ok(tokens)
}

fn parse_number(text: &str, column: usize) -> Result<Decimal, CalculateError> {
    let bad = |message: String| CalculateError::Parse { column, message };
    if text.matches('.').count() > 1 || text == "." {
        return Err(bad(format!("'{text}' is not a number")));
    }
    let parsed = if text.contains(['e', 'E']) {
        Decimal::from_scientific(text)
    } else {
        Decimal::from_str_exact(text)
    };
    parsed.map_err(|_| {
        // Echo a bounded prefix: the literal itself may be 4 KB long.
        let shown: String = text.chars().take(32).collect();
        let more = if text.chars().count() > 32 { "…" } else { "" };
        bad(format!(
            "'{shown}{more}' is not a number that fits in 28 significant digits"
        ))
    })
}

struct Parser {
    chars: Vec<char>,
    tokens: Vec<(Token, usize)>,
    pos: usize,
    depth: usize,
    end_column: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos).map(|(token, _)| token)
    }

    fn column(&self) -> usize {
        self.tokens
            .get(self.pos)
            .map_or(self.end_column, |(_, column)| *column)
    }

    fn error(&self, message: impl Into<String>) -> CalculateError {
        CalculateError::Parse {
            column: self.column(),
            message: message.into(),
        }
    }

    fn descend(&mut self) -> Result<(), CalculateError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(CalculateError::TooDeep);
        }
        Ok(())
    }

    /// sum := product (('+' | '-') product)*
    fn sum(&mut self) -> Result<Decimal, CalculateError> {
        let mut value = self.product()?;
        while let Some(Token::Op(op @ ('+' | '-'))) = self.peek().cloned() {
            self.pos += 1;
            let rhs = self.product()?;
            value = if op == '+' {
                value.checked_add(rhs)
            } else {
                value.checked_sub(rhs)
            }
            .ok_or_else(|| CalculateError::Overflow(op.to_string()))?;
        }
        Ok(value)
    }

    /// product := unary (('*' | '/' | '%') unary)*
    fn product(&mut self) -> Result<Decimal, CalculateError> {
        let mut value = self.unary()?;
        while let Some(Token::Op(op @ ('*' | '/' | '%'))) = self.peek().cloned() {
            let column = self.column();
            // `15%` written as a percent: `%` right after a number, no space.
            let glued = self.pos > 0
                && matches!(self.tokens[self.pos - 1].0, Token::Number(_))
                && column >= 2
                && !self.chars[column - 2].is_whitespace();
            self.pos += 1;
            let signed = matches!(self.peek(), Some(Token::Op('+' | '-')));
            if op == '%' && (self.at_operand_end() || (glued && signed)) {
                return Err(CalculateError::Parse {
                    column,
                    message: "'%' is the remainder operator, not a percent sign; \
                              use percent(part, whole), or divide by 100"
                        .into(),
                });
            }
            let rhs = self.unary()?;
            value = match op {
                '*' => value
                    .checked_mul(rhs)
                    .ok_or_else(|| CalculateError::Overflow("*".into()))?,
                _ if rhs.is_zero() => return Err(CalculateError::DivisionByZero),
                '/' => value
                    .checked_div(rhs)
                    .ok_or_else(|| CalculateError::Overflow("/".into()))?,
                _ => value
                    .checked_rem(rhs)
                    .ok_or_else(|| CalculateError::Overflow("%".into()))?,
            };
        }
        Ok(value)
    }

    /// Whether the next token cannot start an operand, as after `15%`.
    fn at_operand_end(&self) -> bool {
        matches!(
            self.peek(),
            None | Some(Token::RParen | Token::Comma | Token::Op('*' | '/' | '%' | '^'))
        )
    }

    /// unary := ('-' | '+') unary | power
    fn unary(&mut self) -> Result<Decimal, CalculateError> {
        if let Some(Token::Op(op @ ('+' | '-'))) = self.peek().cloned() {
            self.pos += 1;
            self.descend()?;
            let value = self.unary()?;
            self.depth -= 1;
            return Ok(if op == '-' { -value } else { value });
        }
        self.power()
    }

    /// power := primary ('^' unary)?, right-associative, so `-2^2` is -4
    /// and `2^-1` is 0.5.
    fn power(&mut self) -> Result<Decimal, CalculateError> {
        let base = self.primary()?;
        if let Some(Token::Op('^')) = self.peek() {
            let column = self.column();
            self.pos += 1;
            self.descend()?;
            let exponent = self.unary()?;
            self.depth -= 1;
            return pow(base, exponent).map_err(|error| at_column(error, column));
        }
        Ok(base)
    }

    /// primary := number | '(' sum ')' | name '(' args ')'
    fn primary(&mut self) -> Result<Decimal, CalculateError> {
        match self.peek().cloned() {
            Some(Token::Number(value)) => {
                self.pos += 1;
                Ok(value)
            }
            Some(Token::LParen) => {
                self.pos += 1;
                self.descend()?;
                let value = self.sum()?;
                self.depth -= 1;
                self.expect_rparen()?;
                Ok(value)
            }
            Some(Token::Ident(name)) => {
                let column = self.column();
                self.pos += 1;
                if self.peek() != Some(&Token::LParen) {
                    return Err(CalculateError::Parse {
                        column,
                        message: format!(
                            "'{name}' is not a number; only functions are named, \
                             e.g. {name}(…), and there are no variables"
                        ),
                    });
                }
                self.pos += 1;
                self.descend()?;
                let mut args = Vec::new();
                if self.peek() != Some(&Token::RParen) {
                    args.push(self.sum()?);
                    while self.peek() == Some(&Token::Comma) {
                        self.pos += 1;
                        args.push(self.sum()?);
                    }
                }
                self.depth -= 1;
                self.expect_rparen()?;
                call(&name, &args).map_err(|error| at_column(error, column))
            }
            Some(_) => Err(self.error("expected a number, '(' or a function")),
            None => Err(self.error("the expression ends where a number was expected")),
        }
    }

    fn expect_rparen(&mut self) -> Result<(), CalculateError> {
        if self.peek() == Some(&Token::RParen) {
            self.pos += 1;
            Ok(())
        } else {
            Err(self.error("expected ')'"))
        }
    }
}

/// A domain error (a bad argument) reported where it was written.
fn at_column(error: CalculateError, column: usize) -> CalculateError {
    match error {
        CalculateError::Domain(message) => CalculateError::Parse { column, message },
        other => other,
    }
}

fn pow(base: Decimal, exponent: Decimal) -> Result<Decimal, CalculateError> {
    let overflow = || CalculateError::Overflow("^".into());
    if exponent.is_integer() {
        if base.is_zero() && exponent.is_sign_negative() {
            return Err(CalculateError::DivisionByZero);
        }
        let exponent = exponent.to_i64().ok_or_else(overflow)?;
        return base.checked_powi(exponent).ok_or_else(overflow);
    }
    if base.is_sign_negative() {
        return Err(CalculateError::Domain(
            "a negative number to a fractional power has no real value".into(),
        ));
    }
    if base.is_zero() {
        return if exponent.is_sign_negative() {
            Err(CalculateError::DivisionByZero)
        } else {
            Ok(Decimal::ZERO)
        };
    }
    base.checked_powd(exponent).ok_or_else(overflow)
}

/// The functions, by name. Statistics take any number of arguments.
fn call(name: &str, args: &[Decimal]) -> Result<Decimal, CalculateError> {
    let domain = |message: String| CalculateError::Domain(message);
    let overflow = || CalculateError::Overflow(name.to_string());
    let arity = |want: &str| domain(format!("{name}() takes {want}, got {}", args.len()));
    match name {
        "sum" | "mean" | "median" | "min" | "max" | "stdev" | "pstdev" if args.is_empty() => {
            Err(arity("at least one number"))
        }
        "sum" => sum(args).ok_or_else(overflow),
        "mean" => mean(args).ok_or_else(overflow),
        "median" => {
            let mut sorted = args.to_vec();
            sorted.sort();
            let mid = sorted.len() / 2;
            if sorted.len() % 2 == 1 {
                Ok(sorted[mid])
            } else {
                mean(&sorted[mid - 1..=mid]).ok_or_else(overflow)
            }
        }
        "min" => Ok(args.iter().copied().min().unwrap_or_default()),
        "max" => Ok(args.iter().copied().max().unwrap_or_default()),
        "stdev" if args.len() < 2 => Err(arity("at least two numbers")),
        "stdev" => stdev(args, args.len() - 1).ok_or_else(overflow),
        "pstdev" => stdev(args, args.len()).ok_or_else(overflow),
        "abs" => match args {
            [x] => Ok(x.abs()),
            _ => Err(arity("one number")),
        },
        "sqrt" => match args {
            [x] if x.is_zero() => Ok(Decimal::ZERO),
            [x] if x.is_sign_negative() => Err(domain("sqrt() of a negative number".into())),
            [x] => x.sqrt().ok_or_else(overflow),
            _ => Err(arity("one number")),
        },
        "round" => {
            let (value, places) = match args {
                [x] => (*x, Decimal::ZERO),
                [x, places] => (*x, *places),
                _ => return Err(arity("a number and optionally the decimal places")),
            };
            let places = places
                .to_u32()
                .filter(|p| places.is_integer() && *p <= MAX_PLACES)
                .ok_or_else(|| domain(format!("round() places must be 0 to {MAX_PLACES}")))?;
            Ok(value.round_dp_with_strategy(places, RoundingStrategy::MidpointAwayFromZero))
        }
        "percent" => match args {
            [_, whole] if whole.is_zero() => Err(CalculateError::DivisionByZero),
            [part, whole] => part
                .checked_div(*whole)
                .and_then(|ratio| ratio.checked_mul(Decimal::ONE_HUNDRED))
                .ok_or_else(overflow),
            _ => Err(arity("two numbers: part, whole")),
        },
        "change" => match args {
            [old, _] if old.is_zero() => Err(CalculateError::DivisionByZero),
            [old, new] => new
                .checked_sub(*old)
                .and_then(|delta| delta.checked_div(old.abs()))
                .and_then(|ratio| ratio.checked_mul(Decimal::ONE_HUNDRED))
                .ok_or_else(overflow),
            _ => Err(arity("two numbers: old, new")),
        },
        _ => Err(domain(format!(
            "unknown function '{name}'; there are sum, mean, median, min, max, stdev, \
             pstdev, abs, sqrt, round, percent and change"
        ))),
    }
}

fn sum(values: &[Decimal]) -> Option<Decimal> {
    values
        .iter()
        .try_fold(Decimal::ZERO, |acc, value| acc.checked_add(*value))
}

fn mean(values: &[Decimal]) -> Option<Decimal> {
    sum(values)?.checked_div(Decimal::from(values.len()))
}

/// Standard deviation with `divisor` = n - 1 (sample) or n (population).
fn stdev(values: &[Decimal], divisor: usize) -> Option<Decimal> {
    let mean = mean(values)?;
    let squares = values.iter().try_fold(Decimal::ZERO, |acc, value| {
        let deviation = value.checked_sub(mean)?;
        acc.checked_add(deviation.checked_mul(deviation)?)
    })?;
    squares.checked_div(Decimal::from(divisor))?.sqrt()
}

/// Evaluate one expression to an exact decimal.
pub fn evaluate(expression: &str) -> Result<Decimal, CalculateError> {
    if expression.len() > MAX_EXPRESSION_BYTES {
        return Err(CalculateError::TooLong(expression.len()));
    }
    let tokens = tokenize(expression)?;
    if tokens.is_empty() {
        return Err(CalculateError::Empty);
    }
    let mut parser = Parser {
        chars: expression.chars().collect(),
        tokens,
        pos: 0,
        depth: 0,
        end_column: expression.chars().count() + 1,
    };
    let value = parser.sum()?;
    if parser.pos < parser.tokens.len() {
        return Err(parser.error("unexpected input after the end of the expression"));
    }
    Ok(value.normalize())
}

#[derive(Clone, Copy, Debug, Default)]
pub struct CalculateTool;

impl Tool for CalculateTool {
    const NAME: &'static str = "calculate";
    type Error = CalculateError;
    type Args = CalculateArgs;
    type Output = CalculateOutput;

    fn description(&self) -> String {
        "Evaluate an arithmetic expression exactly, in decimal (0.1 + 0.2 = 0.3, up to 28 \
         significant digits). Use it for every number you compute or check instead of mental \
         arithmetic. Operators: + - * / ^ (power) % (remainder) and parentheses. Functions: \
         sum, mean, median, min, max, stdev (sample), pstdev (population) over any number of \
         arguments; abs(x), sqrt(x), round(x, places) (half away from zero); \
         percent(part, whole) = part / whole * 100; change(old, new) = percent change. \
         Example: round(percent(412, 1650), 1). No variables; one expression per call."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "expression": {
                    "type": "string",
                    "description": "The expression, e.g. \"(1820 - 1650) / 1650 * 100\" or \
                                    \"stdev(12.5, 14, 9.75)\"."
                }
            },
            "required": ["expression"]
        })
    }

    /// Keep the real failure text in front of the model: rig's default
    /// `map_error` redacts it to "the tool failed" (AGE-187).
    fn map_error(&self, error: Self::Error) -> ToolExecutionError {
        crate::tools::map_tool_error(Self::NAME, error)
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let result = evaluate(&args.expression)?;
        Ok(CalculateOutput {
            expression: args.expression,
            result: result.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(expression: &str) -> String {
        evaluate(expression)
            .unwrap_or_else(|e| panic!("{expression}: {e}"))
            .to_string()
    }

    fn err(expression: &str) -> CalculateError {
        evaluate(expression).expect_err(expression)
    }

    #[test]
    fn decimal_arithmetic_is_exact() {
        assert_eq!(eval("0.1 + 0.2"), "0.3");
        assert_eq!(eval("1.1 * 3"), "3.3");
        assert_eq!(eval("1 - 0.9"), "0.1");
        assert_eq!(eval("412 / 1650.0 * 100"), "24.96969696969696969696969697");
        assert_eq!(eval("1 / 3"), "0.3333333333333333333333333333");
        assert_eq!(eval("1.5e3 + 2E-2"), "1500.02");
        assert_eq!(eval("  7  "), "7");
    }

    #[test]
    fn precedence_associativity_and_signs() {
        assert_eq!(eval("2 + 3 * 4"), "14");
        assert_eq!(eval("(2 + 3) * 4"), "20");
        assert_eq!(eval("10 - 4 - 3"), "3");
        assert_eq!(eval("2 ^ 3 ^ 2"), "512", "power is right-associative");
        assert_eq!(eval("2 ** 10"), "1024");
        assert_eq!(eval("-2 ^ 2"), "-4");
        assert_eq!(eval("2 ^ -1"), "0.5");
        assert_eq!(eval("--3"), "3");
        assert_eq!(eval("10 % 3"), "1");
        assert_eq!(eval("10%3"), "1");
        assert_eq!(eval("10 %3"), "1");
        assert_eq!(
            eval("10 % -3"),
            "1",
            "a spaced remainder by a signed number"
        );
        assert_eq!(
            eval("(10)%+3"),
            "1",
            "only a literal right before % reads as a percent"
        );
        assert_eq!(eval("-7.5 % 2"), "-1.5");
        assert_eq!(eval("1.5 ^ 2"), "2.25");
        assert_eq!(eval("round(16 ^ 0.5, 10)"), "4");
    }

    #[test]
    fn percentages_and_rounding() {
        assert_eq!(eval("percent(412, 1650)"), "24.96969696969696969696969697");
        assert_eq!(eval("round(percent(412, 1650), 1)"), "25");
        assert_eq!(eval("round(percent(412, 1650), 2)"), "24.97");
        assert_eq!(eval("change(1650, 1820)"), "10.3030303030303030303030303");
        assert_eq!(eval("change(200, 150)"), "-25");
        assert_eq!(eval("change(-200, -100)"), "50");
        assert_eq!(eval("round(2.5)"), "3", "half away from zero");
        assert_eq!(eval("round(-2.5)"), "-3");
        assert_eq!(eval("round(1.005, 2)"), "1.01", "no binary-float surprise");
        assert_eq!(eval("abs(-4.2)"), "4.2");
        assert_eq!(eval("sqrt(2.25)"), "1.5");
        assert_eq!(eval("sqrt(-0)"), "0");
        assert_eq!(eval("sqrt(-0.0)"), "0");
    }

    #[test]
    fn statistics_over_a_list() {
        assert_eq!(eval("sum(1, 2, 3.5)"), "6.5");
        assert_eq!(eval("mean(2, 4, 4, 4, 5, 5, 7, 9)"), "5");
        assert_eq!(eval("median(3, 1, 2)"), "2");
        assert_eq!(eval("median(4, 1, 3, 2)"), "2.5");
        assert_eq!(eval("min(3, -1, 2)"), "-1");
        assert_eq!(eval("max(3, -1, 2)"), "3");
        assert_eq!(eval("pstdev(2, 4, 4, 4, 5, 5, 7, 9)"), "2");
        assert_eq!(eval("round(stdev(2, 4, 4, 4, 5, 5, 7, 9), 6)"), "2.13809");
        assert_eq!(eval("stdev(5, 5)"), "0");
        assert_eq!(eval("mean(sum(1, 2), 3 * 2)"), "4.5");
        assert_eq!(eval("SUM(1, 1)"), "2", "names are case-insensitive");
    }

    #[test]
    fn errors_say_what_and_where() {
        assert_eq!(err("1 / 0"), CalculateError::DivisionByZero);
        assert_eq!(err("5 % 0"), CalculateError::DivisionByZero);
        assert_eq!(err("0 ^ -1"), CalculateError::DivisionByZero);
        assert_eq!(err("percent(1, 0)"), CalculateError::DivisionByZero);
        assert_eq!(err("change(0, 5)"), CalculateError::DivisionByZero);
        assert_eq!(err(""), CalculateError::Empty);
        assert_eq!(err("   "), CalculateError::Empty);

        let at = |expression: &str| match err(expression) {
            CalculateError::Parse { column, message } => (column, message),
            other => panic!("{expression}: {other:?}"),
        };
        assert_eq!(at("1 + $").0, 5);
        assert_eq!(at("(1 + 2").0, 7, "the end of the input");
        assert_eq!(at("1 2").0, 3);
        assert_eq!(at("1 +").0, 4);
        assert_eq!(at("1..2").0, 1);
        assert!(at("x + 1").1.contains("no variables"));
        let (column, message) = at("2 * foo(1)");
        assert_eq!(column, 5);
        assert!(message.contains("unknown function 'foo'"), "{message}");
        assert!(at("stdev(1)").1.contains("at least two numbers"));
        assert!(at("mean()").1.contains("at least one number"));
        assert!(at("sqrt(-4)").1.contains("negative"));
        assert!(at("(-8) ^ 0.5").1.contains("fractional power"));
        assert!(at("round(1, 1.5)").1.contains("places"));
        assert!(at("round(1, 29)").1.contains("places"));
        let (column, message) = at("15% * 200");
        assert_eq!(column, 3);
        assert!(message.contains("percent(part, whole)"), "{message}");
        assert!(matches!(at("1 + 2%").1, m if m.contains("remainder")));
        for percent in ["15% + 200", "15% - 200", "15%-3", "(15%)", "f(15%, 1)"] {
            let (column, message) = at(percent);
            assert!(message.contains("remainder"), "{percent}: {message}");
            if !percent.starts_with(['(', 'f']) {
                assert_eq!(column, 3, "{percent}");
            }
        }
        let long = "1".repeat(MAX_EXPRESSION_BYTES);
        let message = at(&long).1;
        assert!(message.len() < 120, "{message}");
        assert!(
            message.contains(&format!("'{}…'", "1".repeat(32))),
            "{message}"
        );

        assert!(matches!(
            err("79228162514264337593543950335 * 2"),
            CalculateError::Overflow(_)
        ));
        assert!(matches!(err("10 ^ 29"), CalculateError::Overflow(_)));
        assert!(matches!(
            err("2 ^ 99999999999999999999999"),
            CalculateError::Overflow(_)
        ));
        assert!(matches!(
            err("0.12345678901234567890123456789"),
            CalculateError::Parse { .. }
        ));
    }

    #[test]
    fn input_and_nesting_are_bounded() {
        let long = format!("1{}", "+1".repeat(MAX_EXPRESSION_BYTES));
        assert_eq!(err(&long), CalculateError::TooLong(long.len()));

        let at_limit = format!("1{}", "+1".repeat((MAX_EXPRESSION_BYTES - 1) / 2));
        assert!(at_limit.len() <= MAX_EXPRESSION_BYTES);
        assert_eq!(
            eval(&at_limit),
            ((MAX_EXPRESSION_BYTES - 1) / 2 + 1).to_string()
        );

        let deep = format!(
            "{}1{}",
            "(".repeat(MAX_DEPTH + 1),
            ")".repeat(MAX_DEPTH + 1)
        );
        assert_eq!(err(&deep), CalculateError::TooDeep);
        let ok = format!("{}1{}", "(".repeat(MAX_DEPTH), ")".repeat(MAX_DEPTH));
        assert_eq!(eval(&ok), "1");
        assert_eq!(err(&"-".repeat(1000)), CalculateError::TooDeep);
        assert_eq!(err(&"2^".repeat(1000)), CalculateError::TooDeep);

        // The largest list the size limit allows, and the slowest functions
        // on the extremes of the range, finish without panicking.
        let list = vec!["9.99"; (MAX_EXPRESSION_BYTES - 10) / 5].join(",");
        assert_eq!(eval(&format!("median({list})")), "9.99");
        assert_eq!(eval(&format!("stdev({list})")), "0");
        evaluate("sqrt(79228162514264337593543950335)").unwrap();
        evaluate("sqrt(0.0000000000000000000000000001)").unwrap();
        evaluate("1.0000001 ^ 4294967295").ok();
        evaluate("1.5 ^ 0.123456789").unwrap();
        let _ = evaluate("pstdev(79228162514264337593543950335, -79228162514264337593543950335)");
    }

    #[tokio::test]
    async fn the_tool_returns_the_result_or_the_error() {
        let mut context = ToolContext::new();
        let output = CalculateTool
            .call(
                &mut context,
                CalculateArgs {
                    expression: "0.1 + 0.2".into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(output.result, "0.3");
        assert_eq!(output.expression, "0.1 + 0.2");

        let error = CalculateTool
            .call(
                &mut context,
                CalculateArgs {
                    expression: "1/0".into(),
                },
            )
            .await
            .unwrap_err();
        let mapped = CalculateTool.map_error(error);
        assert!(
            mapped.to_string().contains("calculate: division by zero"),
            "{mapped}"
        );
    }
}

/// The reviewer preset calls `calculate` through the real agent factory and
/// session, against a fake OpenAI-compatible model as a hosted guest's is.
/// Runs in a `--no-default-features` build too, which is what hive builds:
/// there the reviewer has no `query_data`, and still has a calculator.
#[cfg(test)]
mod reviewer_tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use crate::agent_spec::load_agent_spec_from;
    use crate::factories::agent_factory::{AgentBuildContext, AgentServices};
    use crate::services::StreamSurface;
    use crate::session::{AgentSession, AgentSessionConfig, SessionEvent, TurnInput};
    use crate::settings::models::execution_settings::ExecutionSettingsModel;
    use crate::settings::models::models_store::ModelConfig;
    use crate::settings::models::providers_store::{ProviderConfig, ProviderType};
    use crate::testing::fake_model::{FakeDaemon, Reply, Script};

    const MODEL: &str = "age-942-model";

    #[tokio::test]
    async fn a_reviewer_recomputes_a_figure_with_calculate() {
        let _ = crate::init_repositories();
        let daemon = FakeDaemon::scripted(Script::new().route(
            MODEL,
            [
                Reply::tool_call(
                    "calculate",
                    serde_json::json!({"expression": "round(percent(412, 1650), 1)"}),
                ),
                Reply::text("REQUEST_CHANGES\n1. 412 of 1650 is 25.0%, not 27%."),
            ],
        ));
        let model = ModelConfig::new(
            MODEL.to_string(),
            MODEL.to_string(),
            ProviderType::OpenRouter,
            MODEL.to_string(),
        );
        let provider = ProviderConfig::new("Hive".to_string(), ProviderType::OpenRouter)
            .with_api_key("lease-token".to_string())
            .with_base_url(daemon.base_url());
        let workspace = tempfile::tempdir().expect("a workspace");
        let settings = ExecutionSettingsModel {
            workspace_dir: Some(workspace.path().to_string_lossy().into_owned()),
            fetch_enabled: false,
            ..ExecutionSettingsModel::default()
        };
        let spec = load_agent_spec_from("reviewer", None, None)
            .expect("the reviewer preset loads")
            .spec;
        let built = AgentBuildContext::from_spec(
            &spec,
            AgentServices {
                exec_settings: Some(settings.clone()),
                ..AgentServices::default()
            },
        )
        .expect("the spec builds");
        let mut session = AgentSession::new(AgentSessionConfig {
            execution_settings: settings,
            surface: StreamSurface::Headless,
            loop_guard: false,
        });
        session
            .create_conversation(
                "age-942".to_string(),
                "Review".to_string(),
                &model,
                &provider,
                built.context,
            )
            .await
            .expect("the conversation builds");
        let events: Rc<RefCell<Vec<SessionEvent>>> = Rc::default();
        let sink = events.clone();
        session
            .begin_turn(
                TurnInput::text("Review: 412 of 1650 orders were late, which is 27%."),
                move |event| sink.borrow_mut().push(event),
            )
            .expect("the turn starts")
            .await;
        let events = Rc::try_unwrap(events).unwrap().into_inner();
        let errors: Vec<_> = events
            .iter()
            .filter(|event| matches!(event, SessionEvent::Error(_)))
            .collect();
        assert!(errors.is_empty(), "{errors:?}");

        let requests = daemon.requests();
        assert_eq!(requests.len(), 2, "the tool call, then the verdict");
        let tools: Vec<String> = requests[0].json()["tools"]
            .as_array()
            .expect("the request carries tools")
            .iter()
            .filter_map(|tool| tool["function"]["name"].as_str().map(str::to_string))
            .collect();
        assert!(tools.contains(&"calculate".to_string()), "{tools:?}");
        assert!(
            !tools.contains(&"shell_execute".to_string()),
            "the preset disables the shell: {tools:?}"
        );
        assert!(
            cfg!(feature = "duckdb") || !tools.contains(&"query_data".to_string()),
            "no DuckDB, no query_data: {tools:?}"
        );
        let second = String::from_utf8_lossy(&requests[1].body);
        assert!(
            second.contains(r#"\"result\":\"25\""#),
            "the tool's result reaches the model: {second}"
        );
    }
}
