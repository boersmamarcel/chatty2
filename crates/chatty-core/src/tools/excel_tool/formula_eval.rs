//! A small Excel formula evaluator for the cached results `write_excel` and
//! `edit_excel` store (AGE-1790).
//!
//! rust_xlsxwriter stores every formula with a cached value of 0 and asks
//! Excel to recalculate on open. LibreOffice (default settings), attachment
//! previews and readers that trust the cached value (calamine, pandas,
//! openpyxl `data_only`) then show 0 in every formula cell. This module
//! computes each formula's value over a model of the workbook so the writer
//! can store the real result.
//!
//! Supported: numbers, strings, booleans and error literals; cell, range and
//! whole-column references, cross-sheet (`Sheet!A1`, `'My Sheet'!$A$1:$B$9`);
//! `+ - * / ^ %`, unary minus, `&`, comparisons; `SUM`, `AVERAGE`, `MIN`,
//! `MAX`, `COUNT`, `COUNTA`, `IF`, `IFERROR`, `ROUND`, `ABS`, `VLOOKUP`,
//! `SUMIF`, `COUNTIF`. Anything else (another function, a defined name, an
//! array result, a cycle, a reference to a sheet the workbook lacks) makes a
//! cell "not evaluated": the caller keeps its old cached value and lists it.
//! Nothing here panics on a malformed formula, and the work per workbook is
//! bounded (formula length, nesting depth, cell reads).

use std::cmp::Ordering;
use std::collections::{HashMap, VecDeque};

use super::parsing::col_from_letters;

/// Excel's last row, 0-based.
const MAX_ROW: u32 = 1_048_575;
/// Excel's longest formula.
const MAX_FORMULA_LEN: usize = 8_192;
/// Deepest nesting of parentheses, function calls and unary operators.
const MAX_DEPTH: usize = 64;
/// Most binary operators in one formula: a chain like `A1+A2+...` nests
/// one level per operator, and evaluation recurses that deep (about 1 KiB
/// of stack per level in a debug build).
const MAX_OPERATORS: usize = 250;
/// Excel's last column, 0-based.
const MAX_COL: u32 = 16_383;
/// Most non-empty cells one reference hands to a function.
const MAX_AREA_CELLS: usize = 1_000_000;
/// Most cell reads for one workbook; past it, the remaining formulas are
/// "not evaluated" rather than slowing the tool call down.
const READ_BUDGET: u64 = 20_000_000;
/// Most cells named in the warning; the rest are counted.
const MAX_LISTED: usize = 20;

/// An Excel error value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum XlError {
    Div0,
    NA,
    Ref,
    Value,
    Name,
    Num,
    Null,
}

impl XlError {
    const ALL: [XlError; 7] = [
        XlError::Div0,
        XlError::NA,
        XlError::Ref,
        XlError::Value,
        XlError::Name,
        XlError::Num,
        XlError::Null,
    ];

    pub(super) fn as_str(self) -> &'static str {
        match self {
            XlError::Div0 => "#DIV/0!",
            XlError::NA => "#N/A",
            XlError::Ref => "#REF!",
            XlError::Value => "#VALUE!",
            XlError::Name => "#NAME?",
            XlError::Num => "#NUM!",
            XlError::Null => "#NULL!",
        }
    }

    pub(super) fn parse(s: &str) -> Option<XlError> {
        Self::ALL
            .into_iter()
            .find(|e| e.as_str().eq_ignore_ascii_case(s))
    }
}

/// A cell value, as Excel sees it.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum XlValue {
    Empty,
    Number(f64),
    Text(String),
    Bool(bool),
    Error(XlError),
}

impl XlValue {
    /// The value `write_cell_value` writes for a JSON cell.
    pub(super) fn from_json(v: &serde_json::Value) -> XlValue {
        match v {
            serde_json::Value::Null => XlValue::Empty,
            serde_json::Value::Number(n) => XlValue::Number(n.as_f64().unwrap_or(0.0)),
            serde_json::Value::Bool(b) => XlValue::Bool(*b),
            serde_json::Value::String(s) => XlValue::Text(s.clone()),
            other => XlValue::Text(other.to_string()),
        }
    }

    /// The result string rust_xlsxwriter's `Formula::set_result` takes.
    pub(super) fn to_cached_result(&self) -> String {
        match self {
            XlValue::Empty => "0".to_string(),
            XlValue::Number(n) => format!("{}", if *n == 0.0 { 0.0 } else { *n }),
            XlValue::Text(s) => s.clone(),
            XlValue::Bool(true) => "TRUE".to_string(),
            XlValue::Bool(false) => "FALSE".to_string(),
            XlValue::Error(e) => e.as_str().to_string(),
        }
    }
}

/// What a cell holds before evaluation.
pub(super) enum CellInput {
    Value(XlValue),
    Formula(String),
}

struct SheetModel {
    name: String,
    cells: HashMap<(u32, u16), CellInput>,
    max_row: u32,
    max_col: u16,
}

/// The cells of a workbook, by sheet, as the writer will store them.
#[derive(Default)]
pub(super) struct WorkbookModel {
    sheets: Vec<SheetModel>,
}

impl WorkbookModel {
    /// Add a sheet; returns its index.
    pub(super) fn add_sheet(&mut self, name: &str) -> usize {
        self.sheets.push(SheetModel {
            name: name.to_string(),
            cells: HashMap::new(),
            max_row: 0,
            max_col: 0,
        });
        self.sheets.len() - 1
    }

    /// Set a cell (0-based row and column); an empty value clears it.
    pub(super) fn set(&mut self, sheet: usize, row: u32, col: u16, input: CellInput) {
        let Some(s) = self.sheets.get_mut(sheet) else {
            return;
        };
        if matches!(input, CellInput::Value(XlValue::Empty)) {
            s.cells.remove(&(row, col));
            return;
        }
        s.max_row = s.max_row.max(row);
        s.max_col = s.max_col.max(col);
        s.cells.insert((row, col), input);
    }

    fn sheet_index(&self, name: &str) -> Option<usize> {
        self.sheets
            .iter()
            .position(|s| s.name.eq_ignore_ascii_case(name))
    }
}

/// The computed result of every formula that could be evaluated.
#[derive(Default)]
pub(super) struct Evaluation {
    results: HashMap<(usize, u32, u16), XlValue>,
    /// Formula cells with no computed result, as `Sheet!A1`, in order.
    unevaluated: Vec<(usize, u32, u16)>,
    sheet_names: Vec<String>,
}

impl Evaluation {
    /// The computed result of the formula at (sheet, row, col), if any.
    pub(super) fn result(&self, sheet: usize, row: u32, col: u16) -> Option<&XlValue> {
        self.results.get(&(sheet, row, col))
    }

    /// Every computed result of one sheet.
    pub(super) fn sheet_results(
        &self,
        sheet: usize,
    ) -> impl Iterator<Item = ((u32, u16), &XlValue)> + '_ {
        self.results
            .iter()
            .filter(move |((s, _, _), _)| *s == sheet)
            .map(|((_, r, c), v)| ((*r, *c), v))
    }

    /// rust_xlsxwriter picks a result's type from its text: "" becomes 0,
    /// and text that parses as a number, TRUE/FALSE or an error is stored
    /// as that type. Such a text result cannot be stored as text: count
    /// those cells as not evaluated.
    pub(super) fn demote_unstorable_text(&mut self) {
        fn unstorable(s: &str) -> bool {
            s.is_empty()
                || s.parse::<f64>().is_ok()
                || matches!(s, "TRUE" | "FALSE" | "#GETTING_DATA")
                || XlError::ALL.iter().any(|e| e.as_str() == s)
        }
        let empty: Vec<_> = self
            .results
            .iter()
            .filter(|(_, v)| matches!(v, XlValue::Text(s) if unstorable(s)))
            .map(|(k, _)| *k)
            .collect();
        for k in empty {
            self.results.remove(&k);
            self.unevaluated.push(k);
        }
        self.unevaluated.sort_unstable();
    }

    /// The not-evaluated cells, as `Sheet!A1`.
    pub(super) fn unevaluated_cells(&self) -> Vec<String> {
        self.unevaluated
            .iter()
            .map(|&(s, r, c)| cell_name(&self.sheet_names[s], r, c))
            .collect()
    }

    /// "N formula cell(s) could not be precomputed: Sheet!A1, ..."; `None`
    /// when every formula was evaluated.
    pub(super) fn summary(&self) -> Option<String> {
        if self.unevaluated.is_empty() {
            return None;
        }
        let names = self.unevaluated_cells();
        let mut list = names
            .iter()
            .take(MAX_LISTED)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        if names.len() > MAX_LISTED {
            list.push_str(&format!(", and {} more", names.len() - MAX_LISTED));
        }
        Some(format!(
            "{} formula cell(s) could not be precomputed: {}",
            names.len(),
            list
        ))
    }
}

/// `Sheet!A1`, quoting a sheet name that needs it.
fn cell_name(sheet: &str, row: u32, col: u16) -> String {
    let plain = !sheet.is_empty()
        && !sheet.starts_with(|c: char| c.is_ascii_digit())
        && sheet.chars().all(|c| c.is_alphanumeric() || c == '_');
    let sheet = if plain {
        sheet.to_string()
    } else {
        format!("'{}'", sheet.replace('\'', "''"))
    };
    format!("{sheet}!{}{}", col_letters(col), row + 1)
}

pub(super) fn col_letters(col: u16) -> String {
    let mut n = u32::from(col) + 1;
    let mut out = Vec::new();
    while n > 0 {
        let rem = (n - 1) % 26;
        out.push(b'A' + rem as u8);
        n = (n - 1) / 26;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

/// Evaluate every formula in `model`, in dependency order.
///
/// 1. Parse each formula; one that does not parse (or uses something this
///    evaluator does not support) is not evaluated.
/// 2. Find, for each formula, the formula cells it reads.
/// 3. Evaluate in topological order (Kahn). A formula whose input was not
///    evaluated is not evaluated either; a cycle never empties, so its cells
///    and everything that reads them are not evaluated.
pub(super) fn evaluate(model: &WorkbookModel) -> Evaluation {
    // PHASE 1: collect and parse the formulas.
    let mut nodes: Vec<(usize, u32, u16)> = Vec::new();
    let mut parsed: Vec<Option<Expr>> = Vec::new();
    for (s, sheet) in model.sheets.iter().enumerate() {
        let mut cells: Vec<_> = sheet
            .cells
            .iter()
            .filter_map(|(&(r, c), input)| match input {
                CellInput::Formula(f) => Some((r, c, f.as_str())),
                CellInput::Value(_) => None,
            })
            .collect();
        cells.sort_unstable_by_key(|&(r, c, _)| (r, c));
        for (r, c, f) in cells {
            nodes.push((s, r, c));
            parsed.push(parse_formula(f, model, s));
        }
    }
    let index: HashMap<(usize, u32, u16), usize> =
        nodes.iter().enumerate().map(|(i, k)| (*k, i)).collect();
    let mut by_sheet: Vec<Vec<usize>> = vec![Vec::new(); model.sheets.len()];
    for (i, &(s, _, _)) in nodes.iter().enumerate() {
        by_sheet[s].push(i);
    }

    // PHASE 2: dependencies between formula cells.
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
    let mut indegree: Vec<usize> = vec![0; nodes.len()];
    for (i, expr) in parsed.iter().enumerate() {
        let Some(expr) = expr else { continue };
        let mut areas = Vec::new();
        expr.areas(&mut areas);
        let mut deps = Vec::new();
        for a in areas {
            let Some((r1, c1, r2, c2)) = a.clamped(model) else {
                continue;
            };
            let cells = u64::from(r2 - r1 + 1) * (u64::from(c2 - c1) + 1);
            if cells <= by_sheet[a.sheet].len() as u64 {
                for r in r1..=r2 {
                    for c in c1..=c2 {
                        if let Some(&j) = index.get(&(a.sheet, r, c)) {
                            deps.push(j);
                        }
                    }
                }
            } else {
                deps.extend(by_sheet[a.sheet].iter().copied().filter(|&j| {
                    let (_, r, c) = nodes[j];
                    (r1..=r2).contains(&r) && (c1..=c2).contains(&c)
                }));
            }
        }
        deps.sort_unstable();
        deps.dedup();
        indegree[i] = deps.len();
        for j in deps {
            dependents[j].push(i);
        }
    }

    // PHASE 3: evaluate in dependency order.
    let mut results: HashMap<(usize, u32, u16), XlValue> = HashMap::new();
    let mut failed = vec![false; nodes.len()];
    let mut done = vec![false; nodes.len()];
    let mut queue: VecDeque<usize> = (0..nodes.len()).filter(|&i| indegree[i] == 0).collect();
    let mut ctx = Ctx {
        model,
        results: &mut results,
        budget: READ_BUDGET,
    };
    while let Some(i) = queue.pop_front() {
        done[i] = true;
        if !failed[i] {
            let value = parsed[i]
                .as_ref()
                .and_then(|e| ctx.eval(e).ok())
                .map(|v| match v {
                    XlValue::Empty => XlValue::Number(0.0),
                    v => v,
                });
            match value {
                Some(v) => {
                    ctx.results.insert(nodes[i], v);
                }
                None => failed[i] = true,
            }
        }
        for &d in &dependents[i] {
            if failed[i] {
                failed[d] = true;
            }
            indegree[d] -= 1;
            if indegree[d] == 0 {
                queue.push_back(d);
            }
        }
    }

    let unevaluated = nodes
        .iter()
        .enumerate()
        .filter(|&(i, _)| failed[i] || !done[i])
        .map(|(_, k)| *k)
        .collect();
    Evaluation {
        results,
        unevaluated,
        sheet_names: model.sheets.iter().map(|s| s.name.clone()).collect(),
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// A rectangle of cells on one sheet, 0-based, inclusive.
#[derive(Clone, Copy, Debug)]
struct Area {
    sheet: usize,
    r1: u32,
    c1: u16,
    r2: u32,
    c2: u16,
}

impl Area {
    fn is_cell(&self) -> bool {
        self.r1 == self.r2 && self.c1 == self.c2
    }

    /// The part of the area inside the sheet's used cells; `None` if none.
    fn clamped(&self, model: &WorkbookModel) -> Option<(u32, u16, u32, u16)> {
        let sheet = model.sheets.get(self.sheet)?;
        let r2 = self.r2.min(sheet.max_row);
        let c2 = self.c2.min(sheet.max_col);
        (self.r1 <= r2 && self.c1 <= c2).then_some((self.r1, self.c1, r2, c2))
    }
}

#[derive(Clone, Copy, Debug)]
enum Func {
    Sum,
    Average,
    Min,
    Max,
    Count,
    CountA,
    If,
    IfError,
    Round,
    Abs,
    VLookup,
    SumIf,
    CountIf,
}

impl Func {
    /// The function and its (min, max) argument count.
    fn lookup(name: &str) -> Option<(Func, usize, usize)> {
        let upper = name.to_ascii_uppercase();
        let upper = upper
            .strip_prefix("_XLFN.")
            .or_else(|| upper.strip_prefix("_XLWS."))
            .unwrap_or(&upper);
        Some(match upper {
            "SUM" => (Func::Sum, 1, 255),
            "AVERAGE" => (Func::Average, 1, 255),
            "MIN" => (Func::Min, 1, 255),
            "MAX" => (Func::Max, 1, 255),
            "COUNT" => (Func::Count, 1, 255),
            "COUNTA" => (Func::CountA, 1, 255),
            "IF" => (Func::If, 2, 3),
            "IFERROR" => (Func::IfError, 2, 2),
            "ROUND" => (Func::Round, 2, 2),
            "ABS" => (Func::Abs, 1, 1),
            "VLOOKUP" => (Func::VLookup, 3, 4),
            "SUMIF" => (Func::SumIf, 2, 3),
            "COUNTIF" => (Func::CountIf, 2, 2),
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug)]
enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Pow,
    Concat,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug)]
enum Expr {
    Lit(XlValue),
    /// An omitted argument, as in `IF(A1,,1)`.
    Missing,
    Ref(Area),
    Neg(Box<Expr>),
    Percent(Box<Expr>),
    Bin(BinOp, Box<Expr>, Box<Expr>),
    Call(Func, Vec<Expr>),
}

impl Expr {
    fn areas(&self, out: &mut Vec<Area>) {
        match self {
            Expr::Ref(a) => out.push(*a),
            Expr::Neg(e) | Expr::Percent(e) => e.areas(out),
            Expr::Bin(_, a, b) => {
                a.areas(out);
                b.areas(out);
            }
            Expr::Call(_, args) => args.iter().for_each(|a| a.areas(out)),
            Expr::Lit(_) | Expr::Missing => {}
        }
    }
}

/// Parse a formula as rust_xlsxwriter's `Formula::new` reads it (optional
/// `{`, `=`, `}`). `None` if it does not parse or uses something
/// unsupported.
fn parse_formula(text: &str, model: &WorkbookModel, sheet: usize) -> Option<Expr> {
    if text.len() > MAX_FORMULA_LEN {
        return None;
    }
    let t = text.trim();
    let t = t.strip_prefix('{').unwrap_or(t);
    let t = t.strip_prefix('=').unwrap_or(t);
    let t = t.strip_suffix('}').unwrap_or(t);
    let mut p = Parser {
        s: t.chars().collect(),
        pos: 0,
        model,
        sheet,
        depth: 0,
        operators: 0,
    };
    let expr = p.comparison().ok()?;
    p.ws();
    (p.pos == p.s.len()).then_some(expr)
}

struct Parser<'a> {
    s: Vec<char>,
    pos: usize,
    model: &'a WorkbookModel,
    sheet: usize,
    depth: usize,
    operators: usize,
}

type PResult<T> = Result<T, ()>;

impl Parser<'_> {
    fn ws(&mut self) {
        while self.s.get(self.pos).is_some_and(|c| c.is_whitespace()) {
            self.pos += 1;
        }
    }

    fn peek(&mut self) -> Option<char> {
        self.ws();
        self.s.get(self.pos).copied()
    }

    fn eat(&mut self, text: &str) -> bool {
        self.ws();
        let n = text.chars().count();
        if self.pos + n <= self.s.len()
            && self.s[self.pos..self.pos + n]
                .iter()
                .copied()
                .eq(text.chars())
        {
            self.pos += n;
            true
        } else {
            false
        }
    }

    fn bin(&mut self, op: BinOp, lhs: Expr, rhs: Expr) -> PResult<Expr> {
        self.operators += 1;
        if self.operators > MAX_OPERATORS {
            return Err(());
        }
        Ok(Expr::Bin(op, Box::new(lhs), Box::new(rhs)))
    }

    fn enter(&mut self) -> PResult<()> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            Err(())
        } else {
            Ok(())
        }
    }

    fn comparison(&mut self) -> PResult<Expr> {
        let mut lhs = self.concat()?;
        loop {
            let op = if self.eat("<=") {
                BinOp::Le
            } else if self.eat(">=") {
                BinOp::Ge
            } else if self.eat("<>") {
                BinOp::Ne
            } else if self.eat("<") {
                BinOp::Lt
            } else if self.eat(">") {
                BinOp::Gt
            } else if self.eat("=") {
                BinOp::Eq
            } else {
                return Ok(lhs);
            };
            let rhs = self.concat()?;
            lhs = self.bin(op, lhs, rhs)?;
        }
    }

    fn concat(&mut self) -> PResult<Expr> {
        let mut lhs = self.additive()?;
        while self.eat("&") {
            let rhs = self.additive()?;
            lhs = self.bin(BinOp::Concat, lhs, rhs)?;
        }
        Ok(lhs)
    }

    fn additive(&mut self) -> PResult<Expr> {
        let mut lhs = self.multiplicative()?;
        loop {
            let op = if self.eat("+") {
                BinOp::Add
            } else if self.eat("-") {
                BinOp::Sub
            } else {
                return Ok(lhs);
            };
            let rhs = self.multiplicative()?;
            lhs = self.bin(op, lhs, rhs)?;
        }
    }

    fn multiplicative(&mut self) -> PResult<Expr> {
        let mut lhs = self.power()?;
        loop {
            let op = if self.eat("*") {
                BinOp::Mul
            } else if self.eat("/") {
                BinOp::Div
            } else {
                return Ok(lhs);
            };
            let rhs = self.power()?;
            lhs = self.bin(op, lhs, rhs)?;
        }
    }

    fn power(&mut self) -> PResult<Expr> {
        let mut lhs = self.unary()?;
        while self.eat("^") {
            let rhs = self.unary()?;
            lhs = self.bin(BinOp::Pow, lhs, rhs)?;
        }
        Ok(lhs)
    }

    /// Excel binds unary minus tighter than `^`: `-2^2` is 4.
    fn unary(&mut self) -> PResult<Expr> {
        self.enter()?;
        let expr = if self.eat("-") {
            Expr::Neg(Box::new(self.unary()?))
        } else if self.eat("+") {
            self.unary()?
        } else {
            let mut e = self.primary()?;
            while self.eat("%") {
                e = Expr::Percent(Box::new(e));
            }
            e
        };
        self.depth -= 1;
        Ok(expr)
    }

    fn primary(&mut self) -> PResult<Expr> {
        match self.peek().ok_or(())? {
            '(' => {
                self.pos += 1;
                self.enter()?;
                let e = self.comparison()?;
                self.depth -= 1;
                if !self.eat(")") {
                    return Err(());
                }
                Ok(e)
            }
            '"' => self.string().map(|s| Expr::Lit(XlValue::Text(s))),
            '#' => self.error_literal(),
            '\'' => {
                let name = self.quoted_sheet()?;
                self.sheet_ref(&name)
            }
            c if c.is_ascii_digit() || c == '.' => self.number(),
            c if c.is_alphabetic() || c == '_' || c == '$' || c == '\\' => {
                let word = self.word();
                if self.s.get(self.pos) == Some(&'!') {
                    self.pos += 1;
                    return self.sheet_ref(&word);
                }
                if self.s.get(self.pos) == Some(&'(') {
                    self.pos += 1;
                    return self.call(&word);
                }
                if word.eq_ignore_ascii_case("TRUE") {
                    return Ok(Expr::Lit(XlValue::Bool(true)));
                }
                if word.eq_ignore_ascii_case("FALSE") {
                    return Ok(Expr::Lit(XlValue::Bool(false)));
                }
                self.area(&word, self.sheet)
            }
            _ => Err(()),
        }
    }

    fn word(&mut self) -> String {
        let start = self.pos;
        while self
            .s
            .get(self.pos)
            .is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '$' | '\\'))
        {
            self.pos += 1;
        }
        self.s[start..self.pos].iter().collect()
    }

    fn string(&mut self) -> PResult<String> {
        self.pos += 1; // opening quote
        let mut out = String::new();
        loop {
            match self.s.get(self.pos) {
                None => return Err(()),
                Some('"') if self.s.get(self.pos + 1) == Some(&'"') => {
                    out.push('"');
                    self.pos += 2;
                }
                Some('"') => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some(&c) => {
                    out.push(c);
                    self.pos += 1;
                }
            }
        }
    }

    fn quoted_sheet(&mut self) -> PResult<String> {
        self.pos += 1; // opening quote
        let mut out = String::new();
        loop {
            match self.s.get(self.pos) {
                None => return Err(()),
                Some('\'') if self.s.get(self.pos + 1) == Some(&'\'') => {
                    out.push('\'');
                    self.pos += 2;
                }
                Some('\'') => {
                    self.pos += 1;
                    break;
                }
                Some(&c) => {
                    out.push(c);
                    self.pos += 1;
                }
            }
        }
        if self.s.get(self.pos) != Some(&'!') {
            return Err(());
        }
        self.pos += 1;
        Ok(out)
    }

    fn error_literal(&mut self) -> PResult<Expr> {
        for e in XlError::ALL {
            let text = e.as_str();
            let n = text.chars().count();
            if self.pos + n <= self.s.len() {
                let candidate: String = self.s[self.pos..self.pos + n].iter().collect();
                if candidate.eq_ignore_ascii_case(text) {
                    self.pos += n;
                    return Ok(Expr::Lit(XlValue::Error(e)));
                }
            }
        }
        Err(())
    }

    fn number(&mut self) -> PResult<Expr> {
        let start = self.pos;
        while self
            .s
            .get(self.pos)
            .is_some_and(|c| c.is_ascii_digit() || *c == '.')
        {
            self.pos += 1;
        }
        if self.s.get(self.pos).is_some_and(|c| *c == 'e' || *c == 'E') {
            let mut p = self.pos + 1;
            if self.s.get(p).is_some_and(|c| *c == '+' || *c == '-') {
                p += 1;
            }
            if self.s.get(p).is_some_and(|c| c.is_ascii_digit()) {
                self.pos = p;
                while self.s.get(self.pos).is_some_and(|c| c.is_ascii_digit()) {
                    self.pos += 1;
                }
            }
        }
        let text: String = self.s[start..self.pos].iter().collect();
        let n: f64 = text.parse().map_err(|_| ())?;
        if n.is_finite() {
            Ok(Expr::Lit(XlValue::Number(n)))
        } else {
            Err(())
        }
    }

    /// A reference after `Sheet!`.
    fn sheet_ref(&mut self, name: &str) -> PResult<Expr> {
        let sheet = self.model.sheet_index(name).ok_or(())?;
        let word = self.word();
        self.area(&word, sheet)
    }

    /// `A1`, `A1:B9` or `A:C` starting with `word`, on `sheet`.
    fn area(&mut self, word: &str, sheet: usize) -> PResult<Expr> {
        if let Some((r1, c1)) = cell_ref(word) {
            if self.s.get(self.pos) == Some(&':') {
                self.pos += 1;
                let second = self.word();
                let (r2, c2) = cell_ref(&second).ok_or(())?;
                return Ok(Expr::Ref(Area {
                    sheet,
                    r1: r1.min(r2),
                    c1: c1.min(c2),
                    r2: r1.max(r2),
                    c2: c1.max(c2),
                }));
            }
            return Ok(Expr::Ref(Area {
                sheet,
                r1,
                c1,
                r2: r1,
                c2: c1,
            }));
        }
        if let Some(c1) = col_ref(word)
            && self.s.get(self.pos) == Some(&':')
        {
            self.pos += 1;
            let second = self.word();
            let c2 = col_ref(&second).ok_or(())?;
            return Ok(Expr::Ref(Area {
                sheet,
                r1: 0,
                c1: c1.min(c2),
                r2: MAX_ROW,
                c2: c1.max(c2),
            }));
        }
        // A defined name, or something else this evaluator does not know.
        Err(())
    }

    fn call(&mut self, name: &str) -> PResult<Expr> {
        let (func, min, max) = Func::lookup(name).ok_or(())?;
        self.enter()?;
        let mut args = Vec::new();
        if !self.eat(")") {
            loop {
                let arg = match self.peek() {
                    Some(',') | Some(')') => Expr::Missing,
                    _ => self.comparison()?,
                };
                args.push(arg);
                if self.eat(",") {
                    continue;
                }
                if self.eat(")") {
                    break;
                }
                return Err(());
            }
        }
        self.depth -= 1;
        if args.len() < min || args.len() > max {
            return Err(());
        }
        Ok(Expr::Call(func, args))
    }
}

/// `A1`, `$A$1`: (row, col), 0-based.
fn cell_ref(word: &str) -> Option<(u32, u16)> {
    let w = word.replace('$', "").to_ascii_uppercase();
    let split = w.find(|c: char| c.is_ascii_digit())?;
    let (letters, digits) = w.split_at(split);
    if !digits.bytes().all(|b| b.is_ascii_digit()) || word.matches('$').count() > 2 {
        return None;
    }
    let col = col_from_letters(letters)?;
    let row = digits.parse::<u32>().ok()?.checked_sub(1)?;
    (row <= MAX_ROW).then_some((row, col))
}

/// `A`, `$A`: the column, 0-based.
fn col_ref(word: &str) -> Option<u16> {
    let w = word.strip_prefix('$').unwrap_or(word).to_ascii_uppercase();
    col_from_letters(&w)
}

// ---------------------------------------------------------------------------
// Evaluation
// ---------------------------------------------------------------------------

/// The formula cannot be evaluated here (array result, budget spent).
struct Stop;

type EResult<T> = Result<T, Stop>;

struct Ctx<'a> {
    model: &'a WorkbookModel,
    results: &'a mut HashMap<(usize, u32, u16), XlValue>,
    budget: u64,
}

impl Ctx<'_> {
    fn tick(&mut self) -> EResult<()> {
        self.budget = self.budget.checked_sub(1).ok_or(Stop)?;
        Ok(())
    }

    fn cell(&mut self, sheet: usize, row: u32, col: u16) -> EResult<XlValue> {
        self.tick()?;
        let Some(s) = self.model.sheets.get(sheet) else {
            return Err(Stop);
        };
        match s.cells.get(&(row, col)) {
            None => Ok(XlValue::Empty),
            Some(CellInput::Value(v)) => Ok(v.clone()),
            // Dependency order put it here first; if not, give up.
            Some(CellInput::Formula(_)) => {
                self.results.get(&(sheet, row, col)).cloned().ok_or(Stop)
            }
        }
    }

    /// The non-empty cells of `area`, row by row, with their offsets in the
    /// area. Walks the area when it is smaller than the sheet's cell map and
    /// the map otherwise, so a tall sparse sheet costs its cells, not its
    /// bounding box. More than [`MAX_AREA_CELLS`] cells: not evaluated.
    fn area_cells(&mut self, area: &Area) -> EResult<Vec<(u32, u16, XlValue)>> {
        let model = self.model;
        let Some((r1, c1, r2, c2)) = area.clamped(model) else {
            return Ok(Vec::new());
        };
        let sheet = model.sheets.get(area.sheet).ok_or(Stop)?;
        let size = u64::from(r2 - r1 + 1) * (u64::from(c2 - c1) + 1);
        let mut coords: Vec<(u32, u16)> = Vec::new();
        if size <= sheet.cells.len() as u64 {
            for r in r1..=r2 {
                for c in c1..=c2 {
                    self.tick()?;
                    if sheet.cells.contains_key(&(r, c)) {
                        coords.push((r, c));
                    }
                }
            }
        } else {
            for &(r, c) in sheet.cells.keys() {
                self.tick()?;
                if (r1..=r2).contains(&r) && (c1..=c2).contains(&c) {
                    coords.push((r, c));
                }
            }
            coords.sort_unstable();
        }
        if coords.len() > MAX_AREA_CELLS {
            return Err(Stop);
        }
        let mut out = Vec::with_capacity(coords.len());
        for (r, c) in coords {
            let v = self.cell(area.sheet, r, c)?;
            if !matches!(v, XlValue::Empty) {
                out.push((r - area.r1, c - area.c1, v));
            }
        }
        Ok(out)
    }

    fn eval(&mut self, expr: &Expr) -> EResult<XlValue> {
        Ok(match expr {
            Expr::Lit(v) => v.clone(),
            Expr::Missing => XlValue::Empty,
            Expr::Ref(a) if a.is_cell() => self.cell(a.sheet, a.r1, a.c1)?,
            // A range where one value is wanted: an array formula.
            Expr::Ref(_) => return Err(Stop),
            Expr::Neg(e) => {
                let v = self.eval(e)?;
                match to_number(&v)? {
                    Ok(n) => XlValue::Number(-n),
                    Err(e) => XlValue::Error(e),
                }
            }
            Expr::Percent(e) => {
                let v = self.eval(e)?;
                match to_number(&v)? {
                    Ok(n) => XlValue::Number(n / 100.0),
                    Err(e) => XlValue::Error(e),
                }
            }
            Expr::Bin(op, a, b) => {
                let a = self.eval(a)?;
                let b = self.eval(b)?;
                binary(*op, &a, &b)?
            }
            Expr::Call(f, args) => self.call(*f, args)?,
        })
    }

    fn call(&mut self, func: Func, args: &[Expr]) -> EResult<XlValue> {
        Ok(match func {
            Func::Sum => match self.numbers(args)? {
                Ok(ns) => XlValue::Number(checked_sum(&ns)?),
                Err(e) => XlValue::Error(e),
            },
            Func::Average => match self.numbers(args)? {
                Ok(ns) if ns.is_empty() => XlValue::Error(XlError::Div0),
                Ok(ns) => XlValue::Number(checked_sum(&ns)? / ns.len() as f64),
                Err(e) => XlValue::Error(e),
            },
            Func::Min => match self.numbers(args)? {
                Ok(ns) => XlValue::Number(ns.into_iter().reduce(f64::min).unwrap_or(0.0)),
                Err(e) => XlValue::Error(e),
            },
            Func::Max => match self.numbers(args)? {
                Ok(ns) => XlValue::Number(ns.into_iter().reduce(f64::max).unwrap_or(0.0)),
                Err(e) => XlValue::Error(e),
            },
            Func::Count | Func::CountA => {
                let mut n = 0u32;
                for arg in args {
                    match arg {
                        Expr::Ref(a) => {
                            for (_, _, v) in self.area_cells(a)? {
                                n += u32::from(match func {
                                    Func::Count => matches!(v, XlValue::Number(_)),
                                    _ => true,
                                });
                            }
                        }
                        Expr::Missing => {}
                        e => {
                            let v = self.eval(e)?;
                            n += u32::from(match func {
                                Func::Count => match v {
                                    XlValue::Number(_) | XlValue::Bool(_) => true,
                                    XlValue::Text(s) => text_number(&s)?.is_some(),
                                    _ => false,
                                },
                                _ => true,
                            });
                        }
                    }
                }
                XlValue::Number(f64::from(n))
            }
            Func::If => {
                let cond = self.eval(&args[0])?;
                match to_bool(&cond) {
                    Err(e) => XlValue::Error(e),
                    Ok(true) => self.eval(&args[1])?,
                    Ok(false) => match args.get(2) {
                        Some(e) => self.eval(e)?,
                        None => XlValue::Bool(false),
                    },
                }
            }
            Func::IfError => match self.eval(&args[0])? {
                XlValue::Error(_) => self.eval(&args[1])?,
                v => v,
            },
            Func::Round => {
                let x = self.eval(&args[0])?;
                let d = self.eval(&args[1])?;
                match (to_number(&x)?, to_number(&d)?) {
                    (Ok(x), Ok(d)) => XlValue::Number(round_half_away(x, d.trunc())),
                    (Err(e), _) | (_, Err(e)) => XlValue::Error(e),
                }
            }
            Func::Abs => {
                let v = self.eval(&args[0])?;
                match to_number(&v)? {
                    Ok(n) => XlValue::Number(n.abs()),
                    Err(e) => XlValue::Error(e),
                }
            }
            Func::VLookup => self.vlookup(args)?,
            Func::SumIf | Func::CountIf => self.sum_count_if(func, args)?,
        })
    }

    /// The numbers SUM/AVERAGE/MIN/MAX take: from a reference only numbers
    /// (text, booleans and blanks are skipped); a direct argument is
    /// coerced. The first error wins.
    fn numbers(&mut self, args: &[Expr]) -> EResult<Result<Vec<f64>, XlError>> {
        let mut out = Vec::new();
        for arg in args {
            match arg {
                Expr::Ref(a) => {
                    for (_, _, v) in self.area_cells(a)? {
                        match v {
                            XlValue::Number(n) => out.push(n),
                            XlValue::Error(e) => return Ok(Err(e)),
                            _ => {}
                        }
                    }
                }
                Expr::Missing => {}
                e => match self.eval(e)? {
                    XlValue::Empty => {}
                    v => match to_number(&v)? {
                        Ok(n) => out.push(n),
                        Err(e) => return Ok(Err(e)),
                    },
                },
            }
        }
        Ok(Ok(out))
    }

    /// VLOOKUP. Exact match: numbers, booleans, and text (case-insensitive,
    /// with wildcards). Approximate match only over a number key column
    /// that is fully filled and in non-decreasing order: anything else
    /// depends on Excel's binary search or collation, so it is not
    /// evaluated.
    fn vlookup(&mut self, args: &[Expr]) -> EResult<XlValue> {
        let Expr::Ref(table) = &args[1] else {
            return Err(Stop);
        };
        let lookup = self.eval(&args[0])?;
        match lookup {
            XlValue::Error(e) => return Ok(XlValue::Error(e)),
            XlValue::Empty => return Err(Stop),
            _ => {}
        }
        let col = self.eval(&args[2])?;
        let col = match to_number(&col)? {
            Ok(n) => n.trunc(),
            Err(e) => return Ok(XlValue::Error(e)),
        };
        let approximate = match args.get(3) {
            None | Some(Expr::Missing) => true,
            Some(e) => match to_bool(&self.eval(e)?) {
                Ok(b) => b,
                Err(e) => return Ok(XlValue::Error(e)),
            },
        };
        let width = f64::from(table.c2 - table.c1) + 1.0;
        if col < 1.0 {
            return Ok(XlValue::Error(XlError::Value));
        }
        if col > width {
            return Ok(XlValue::Error(XlError::Ref));
        }
        let first_col = Area {
            c2: table.c1,
            ..*table
        };
        let clamped = first_col.clamped(self.model);
        let mut found = None;
        if approximate {
            let XlValue::Number(x) = lookup else {
                return Err(Stop);
            };
            let Some((r1, _, r2, _)) = clamped else {
                return Err(Stop);
            };
            if r2 < table.r2 {
                // Blank rows below the used area.
                return Err(Stop);
            }
            let mut prev: Option<f64> = None;
            for r in r1..=r2 {
                let XlValue::Number(k) = self.cell(table.sheet, r, table.c1)? else {
                    return Err(Stop);
                };
                if let Some(p) = prev
                    && num_cmp(p, k)? == Ordering::Greater
                {
                    return Err(Stop);
                }
                prev = Some(k);
                if num_cmp(k, x)? != Ordering::Greater {
                    found = Some(r);
                }
            }
        } else {
            let Some((r1, _, r2, _)) = clamped else {
                return Ok(XlValue::Error(XlError::NA));
            };
            for r in r1..=r2 {
                let key = self.cell(table.sheet, r, table.c1)?;
                if lookup_equal(&lookup, &key)? {
                    found = Some(r);
                    break;
                }
            }
        }
        let Some(r) = found else {
            return Ok(XlValue::Error(XlError::NA));
        };
        Ok(
            match self.cell(table.sheet, r, table.c1 + (col as u16 - 1))? {
                XlValue::Empty => XlValue::Number(0.0),
                v => v,
            },
        )
    }

    fn sum_count_if(&mut self, func: Func, args: &[Expr]) -> EResult<XlValue> {
        let Expr::Ref(range) = &args[0] else {
            return Err(Stop);
        };
        let crit = match self.eval(&args[1])? {
            XlValue::Error(e) => return Ok(XlValue::Error(e)),
            v => Criteria::new(v)?,
        };
        if matches!(func, Func::CountIf) {
            let cells = self.area_cells(range)?;
            let mut count = 0u64;
            // Blank cells (not in `cells`) can match too: "", "<>x".
            if crit.matches(&XlValue::Empty)? {
                let size =
                    u64::from(range.r2 - range.r1 + 1) * (u64::from(range.c2 - range.c1) + 1);
                count += size - cells.len() as u64;
            }
            for (_, _, v) in &cells {
                count += u64::from(crit.matches(v)?);
            }
            return Ok(XlValue::Number(count as f64));
        }
        // SUMIF: walk the non-empty cells of the sum area (the criteria
        // range's shape, from the sum range's top-left) and test the
        // criteria cell at the same offset, so a blank criteria cell counts.
        let origin = match args.get(2) {
            None | Some(Expr::Missing) => *range,
            Some(Expr::Ref(a)) => *a,
            Some(_) => return Err(Stop),
        };
        let r2 = origin
            .r1
            .checked_add(range.r2 - range.r1)
            .filter(|r| *r <= MAX_ROW);
        let c2 = origin
            .c1
            .checked_add(range.c2 - range.c1)
            .filter(|c| u32::from(*c) <= MAX_COL);
        let (Some(r2), Some(c2)) = (r2, c2) else {
            return Err(Stop);
        };
        let sum_area = Area { r2, c2, ..origin };
        let mut matched = Vec::new();
        for (dr, dc, v) in self.area_cells(&sum_area)? {
            let n = match v {
                XlValue::Number(n) => Ok(n),
                XlValue::Error(e) => Err(e),
                _ => continue,
            };
            let key = self.cell(range.sheet, range.r1 + dr, range.c1 + dc)?;
            if crit.matches(&key)? {
                match n {
                    Ok(n) => matched.push(n),
                    Err(e) => return Ok(XlValue::Error(e)),
                }
            }
        }
        Ok(XlValue::Number(checked_sum(&matched)?))
    }
}

/// A sum whose result is binary noise (non-zero but below 1e-15 of its
/// largest term, like `0.1 + 0.2 - 0.3`) is not evaluated: Excel may show 0.
fn checked_sum(ns: &[f64]) -> EResult<f64> {
    let sum: f64 = ns.iter().sum();
    let largest = ns.iter().fold(0.0f64, |m, n| m.max(n.abs()));
    if sum != 0.0 && sum.abs() < 1e-15 * largest {
        return Err(Stop);
    }
    Ok(sum)
}

fn binary(op: BinOp, a: &XlValue, b: &XlValue) -> EResult<XlValue> {
    if let XlValue::Error(e) = a {
        return Ok(XlValue::Error(*e));
    }
    if let XlValue::Error(e) = b {
        return Ok(XlValue::Error(*e));
    }
    Ok(match op {
        BinOp::Concat => XlValue::Text(to_text(a)? + &to_text(b)?),
        BinOp::Eq | BinOp::Ne => {
            let equal = compare(a, b, false)? == Ordering::Equal;
            XlValue::Bool(equal == matches!(op, BinOp::Eq))
        }
        BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
            let o = compare(a, b, true)?;
            XlValue::Bool(match op {
                BinOp::Lt => o == Ordering::Less,
                BinOp::Le => o != Ordering::Greater,
                BinOp::Gt => o == Ordering::Greater,
                _ => o != Ordering::Less,
            })
        }
        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Pow => {
            let x = match to_number(a)? {
                Ok(x) => x,
                Err(e) => return Ok(XlValue::Error(e)),
            };
            let y = match to_number(b)? {
                Ok(y) => y,
                Err(e) => return Ok(XlValue::Error(e)),
            };
            let n = match op {
                BinOp::Add => checked_sum(&[x, y])?,
                BinOp::Sub => checked_sum(&[x, -y])?,
                BinOp::Mul => x * y,
                BinOp::Div if y == 0.0 => return Ok(XlValue::Error(XlError::Div0)),
                BinOp::Div => x / y,
                BinOp::Pow if x == 0.0 && y == 0.0 => return Ok(XlValue::Error(XlError::Num)),
                BinOp::Pow if x == 0.0 && y < 0.0 => return Ok(XlValue::Error(XlError::Div0)),
                _ => x.powf(y),
            };
            if n.is_finite() {
                XlValue::Number(n)
            } else {
                XlValue::Error(XlError::Num)
            }
        }
    })
}

/// A number from plain numeric text (`" 12.5 "` yes, `"inf"` no).
fn parse_number(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty()
        || !t
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-'))
    {
        return None;
    }
    t.parse::<f64>().ok().filter(|n| n.is_finite())
}

/// Text as Excel coerces it to a number: `Some` for plain numeric text,
/// `None` for text with no digit (Excel: `#VALUE!`). Text with a digit that
/// is not plain numeric (`"1,000"`, `"$5"`, `"50%"`, a date) may be a number
/// to Excel: not evaluated.
fn text_number(s: &str) -> EResult<Option<f64>> {
    if let Some(n) = parse_number(s) {
        return Ok(Some(n));
    }
    if s.chars().any(|c| c.is_ascii_digit()) {
        return Err(Stop);
    }
    Ok(None)
}

fn to_number(v: &XlValue) -> EResult<Result<f64, XlError>> {
    Ok(match v {
        XlValue::Empty => Ok(0.0),
        XlValue::Number(n) => Ok(*n),
        XlValue::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        XlValue::Text(s) => text_number(s)?.ok_or(XlError::Value),
        XlValue::Error(e) => Err(*e),
    })
}

fn to_bool(v: &XlValue) -> Result<bool, XlError> {
    match v {
        XlValue::Empty => Ok(false),
        XlValue::Number(n) => Ok(*n != 0.0),
        XlValue::Bool(b) => Ok(*b),
        XlValue::Text(s) if s.eq_ignore_ascii_case("TRUE") => Ok(true),
        XlValue::Text(s) if s.eq_ignore_ascii_case("FALSE") => Ok(false),
        XlValue::Text(_) => Err(XlError::Value),
        XlValue::Error(e) => Err(*e),
    }
}

/// A value as `&` shows it. Numbers Excel's General format would show in
/// scientific notation (|n| >= 1e15, or below 1e-4) are not evaluated.
fn to_text(v: &XlValue) -> EResult<String> {
    Ok(match v {
        XlValue::Empty => String::new(),
        XlValue::Number(n) => {
            let a = n.abs();
            if a >= 1e15 || (a != 0.0 && a < 1e-4) {
                return Err(Stop);
            }
            format_general(*n)
        }
        XlValue::Text(s) => s.clone(),
        XlValue::Bool(true) => "TRUE".to_string(),
        XlValue::Bool(false) => "FALSE".to_string(),
        XlValue::Error(e) => e.as_str().to_string(),
    })
}

/// `x` cut to Excel's 15 significant digits, which drops binary noise
/// (`0.1 + 0.2` is 0.3, `2.675 * 100` is 267.5).
fn round15(x: f64) -> f64 {
    if !x.is_finite() || x == 0.0 {
        return x;
    }
    format!("{x:.14e}").parse().unwrap_or(x)
}

/// A number as `&` shows it: 15 significant digits, no trailing zeros.
fn format_general(n: f64) -> String {
    let n = round15(n);
    if n == 0.0 {
        return "0".to_string();
    }
    format!("{n}")
}

/// Excel's ROUND: half away from zero, to `digits` decimals (negative
/// rounds left of the point).
fn round_half_away(x: f64, digits: f64) -> f64 {
    let d = digits.clamp(-308.0, 308.0) as i32;
    if d > 15 {
        return x;
    }
    let out = if d >= 0 {
        let f = 10f64.powi(d);
        round15(x * f).round() / f
    } else {
        let f = 10f64.powi(-d);
        round15(x / f).round() * f
    };
    round15(out)
}

/// Two numbers in order. Numbers that differ only past 15 significant
/// digits (`0.1 + 0.2` against `0.3`) are not evaluated: Excel may call
/// them equal.
fn num_cmp(x: f64, y: f64) -> EResult<Ordering> {
    if x == y {
        return Ok(Ordering::Equal);
    }
    if round15(x) == round15(y) {
        return Err(Stop);
    }
    Ok(x.partial_cmp(&y).unwrap_or(Ordering::Equal))
}

/// Text equality as Excel's `=`: case-insensitive. Only ASCII case is
/// folded here; a non-ASCII pair that differs is not evaluated.
fn text_equal(x: &str, y: &str) -> EResult<bool> {
    if x.eq_ignore_ascii_case(y) {
        return Ok(true);
    }
    if x.is_ascii() && y.is_ascii() {
        return Ok(false);
    }
    Err(Stop)
}

/// Excel's order across kinds: numbers < text < booleans; a blank is 0, ""
/// or FALSE next to the other side. With `ordered`, two different texts are
/// not evaluated (Excel's collation is not plain code-point order) unless
/// one is empty; without it, only equal or not matters.
fn compare(a: &XlValue, b: &XlValue, ordered: bool) -> EResult<Ordering> {
    fn rank(v: &XlValue) -> u8 {
        match v {
            XlValue::Number(_) | XlValue::Empty => 0,
            XlValue::Text(_) => 1,
            XlValue::Bool(_) => 2,
            XlValue::Error(_) => 3,
        }
    }
    let blank_as = |other: &XlValue| match other {
        XlValue::Text(_) => XlValue::Text(String::new()),
        XlValue::Bool(_) => XlValue::Bool(false),
        _ => XlValue::Number(0.0),
    };
    let a = if matches!(a, XlValue::Empty) {
        blank_as(b)
    } else {
        a.clone()
    };
    let b = if matches!(b, XlValue::Empty) {
        blank_as(&a)
    } else {
        b.clone()
    };
    Ok(match (&a, &b) {
        (XlValue::Number(x), XlValue::Number(y)) => num_cmp(*x, *y)?,
        (XlValue::Text(x), XlValue::Text(y)) => {
            if text_equal(x, y)? {
                Ordering::Equal
            } else if x.is_empty() {
                Ordering::Less
            } else if y.is_empty() {
                Ordering::Greater
            } else if ordered {
                return Err(Stop);
            } else {
                Ordering::Less
            }
        }
        (XlValue::Bool(x), XlValue::Bool(y)) => x.cmp(y),
        _ => rank(&a).cmp(&rank(&b)),
    })
}

fn same_kind(a: &XlValue, b: &XlValue) -> bool {
    std::mem::discriminant(a) == std::mem::discriminant(b)
}

/// VLOOKUP's exact match: same kind, text case-insensitive with `*`/`?`.
fn lookup_equal(lookup: &XlValue, key: &XlValue) -> EResult<bool> {
    Ok(match (lookup, key) {
        (XlValue::Number(x), XlValue::Number(y)) => num_cmp(*x, *y)? == Ordering::Equal,
        (XlValue::Text(p), XlValue::Text(t)) => wildcard_match(p, t)?,
        (XlValue::Bool(x), XlValue::Bool(y)) => x == y,
        _ => false,
    })
}

/// Match of `text` against `pattern` with `*`, `?` and `~` (escape), ASCII
/// case-insensitive. Iterative, so a long pattern cannot recurse deeply.
/// A miss where either side has non-ASCII text is not evaluated: Excel
/// folds more case than ASCII.
fn wildcard_match(pattern: &str, text: &str) -> EResult<bool> {
    #[derive(Clone, Copy, PartialEq)]
    enum P {
        Any,
        One,
        Lit(char),
    }
    let mut pat = Vec::new();
    let mut chars = pattern.chars().map(|c| c.to_ascii_lowercase());
    while let Some(c) = chars.next() {
        pat.push(match c {
            '~' => match chars.next() {
                Some(n) => P::Lit(n),
                None => P::Lit('~'),
            },
            '*' => P::Any,
            '?' => P::One,
            c => P::Lit(c),
        });
    }
    let chars: Vec<char> = text.chars().map(|c| c.to_ascii_lowercase()).collect();
    let (mut p, mut t) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    let mut matched = true;
    while t < chars.len() {
        match pat.get(p) {
            Some(P::Any) => {
                star = Some((p, t));
                p += 1;
            }
            Some(P::One) => {
                p += 1;
                t += 1;
            }
            Some(P::Lit(c)) if *c == chars[t] => {
                p += 1;
                t += 1;
            }
            _ => match star {
                Some((sp, st)) => {
                    p = sp + 1;
                    t = st + 1;
                    star = Some((sp, st + 1));
                }
                None => {
                    matched = false;
                    break;
                }
            },
        }
    }
    let matched = matched && pat[p..].iter().all(|x| *x == P::Any);
    if !matched && !(pattern.is_ascii() && text.is_ascii()) {
        return Err(Stop);
    }
    Ok(matched)
}

#[derive(Clone, Copy, PartialEq)]
enum CritOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// A SUMIF/COUNTIF criterion: `5`, `"apples"`, `">=10"`, `"<>"`, `"a*"`.
struct Criteria {
    op: CritOp,
    value: XlValue,
}

impl Criteria {
    /// A blank criterion, or text with a digit that is not plain numeric
    /// (`">1,000"`), is not evaluated.
    fn new(v: XlValue) -> EResult<Criteria> {
        let s = match v {
            XlValue::Text(s) => s,
            XlValue::Empty => return Err(Stop),
            v => {
                return Ok(Criteria {
                    op: CritOp::Eq,
                    value: v,
                });
            }
        };
        let (op, rest) = [
            ("<=", CritOp::Le),
            (">=", CritOp::Ge),
            ("<>", CritOp::Ne),
            ("<", CritOp::Lt),
            (">", CritOp::Gt),
            ("=", CritOp::Eq),
        ]
        .into_iter()
        .find_map(|(p, op)| s.strip_prefix(p).map(|rest| (op, rest)))
        .unwrap_or((CritOp::Eq, s.as_str()));
        let value = if let Some(n) = text_number(rest)? {
            XlValue::Number(n)
        } else if rest.eq_ignore_ascii_case("TRUE") {
            XlValue::Bool(true)
        } else if rest.eq_ignore_ascii_case("FALSE") {
            XlValue::Bool(false)
        } else {
            XlValue::Text(rest.to_string())
        };
        Ok(Criteria { op, value })
    }

    fn equal(&self, cell: &XlValue) -> EResult<bool> {
        Ok(match (&self.value, cell) {
            (XlValue::Text(p), XlValue::Empty) => p.is_empty(),
            (XlValue::Number(x), XlValue::Number(y)) => num_cmp(*x, *y)? == Ordering::Equal,
            // Whether Excel counts numeric text against a number: not
            // evaluated.
            (XlValue::Number(_), XlValue::Text(t)) => {
                if text_number(t)?.is_some() {
                    return Err(Stop);
                }
                false
            }
            (XlValue::Text(p), XlValue::Text(t)) => wildcard_match(p, t)?,
            (XlValue::Bool(x), XlValue::Bool(y)) => x == y,
            _ => false,
        })
    }

    fn matches(&self, cell: &XlValue) -> EResult<bool> {
        Ok(match self.op {
            CritOp::Eq => self.equal(cell)?,
            CritOp::Ne => !self.equal(cell)?,
            op => {
                let (XlValue::Number(x), XlValue::Number(y)) = (cell, &self.value) else {
                    // Text or boolean order follows Excel's collation.
                    if same_kind(cell, &self.value) {
                        return Err(Stop);
                    }
                    return Ok(false);
                };
                let o = num_cmp(*x, *y)?;
                match op {
                    CritOp::Lt => o == Ordering::Less,
                    CritOp::Le => o != Ordering::Greater,
                    CritOp::Gt => o == Ordering::Greater,
                    _ => o != Ordering::Less,
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One sheet "S" with `cells` as (A1-style ref, input); "=" starts a formula.
    fn model(sheets: &[(&str, &[(&str, serde_json::Value)])]) -> WorkbookModel {
        let mut m = WorkbookModel::default();
        for (name, cells) in sheets {
            let s = m.add_sheet(name);
            for (r, v) in cells.iter() {
                let (row, col) = cell_ref(r).unwrap();
                let input = match v {
                    serde_json::Value::String(f) if f.starts_with('=') => {
                        CellInput::Formula(f.clone())
                    }
                    v => CellInput::Value(XlValue::from_json(v)),
                };
                m.set(s, row, col, input);
            }
        }
        m
    }

    /// The value of `=formula` in a sheet with A1..A4 = 1,2,3,4, B1 = "x".
    fn eval_one(formula: &str) -> Option<XlValue> {
        let m = model(&[(
            "S",
            &[
                ("A1", 1.into()),
                ("A2", 2.into()),
                ("A3", 3.into()),
                ("A4", 4.into()),
                ("B1", "x".into()),
                ("Z1", formula.into()),
            ],
        )]);
        let e = evaluate(&m);
        e.result(0, 0, 25).cloned()
    }

    fn num(f: &str) -> f64 {
        match eval_one(f) {
            Some(XlValue::Number(n)) => n,
            other => panic!("{f}: expected a number, got {other:?}"),
        }
    }

    #[test]
    fn arithmetic_and_precedence() {
        assert_eq!(num("=1+2*3"), 7.0);
        assert_eq!(num("=(1+2)*3"), 9.0);
        assert_eq!(num("=-2^2"), 4.0);
        assert_eq!(num("=2^3^2"), 64.0);
        assert_eq!(num("=10/4"), 2.5);
        assert_eq!(num("=50%"), 0.5);
        assert_eq!(num("=--A2"), 2.0);
        assert_eq!(num("=A1+A2*$A$3"), 7.0);
        assert_eq!(num("=1.5e2"), 150.0);
        assert_eq!(eval_one("=1/0"), Some(XlValue::Error(XlError::Div0)));
        assert_eq!(eval_one("=B1+1"), Some(XlValue::Error(XlError::Value)));
        assert_eq!(eval_one("=C9"), Some(XlValue::Number(0.0)));
    }

    #[test]
    fn text_booleans_and_comparisons() {
        assert_eq!(
            eval_one("=\"a\"&\"b\"&A1"),
            Some(XlValue::Text("ab1".into()))
        );
        assert_eq!(
            eval_one("=\"say \"\"hi\"\"\""),
            Some(XlValue::Text("say \"hi\"".into()))
        );
        assert_eq!(eval_one("=0.1+0.2&\"\""), Some(XlValue::Text("0.3".into())));
        assert_eq!(eval_one("=A1<A2"), Some(XlValue::Bool(true)));
        assert_eq!(eval_one("=B1=\"X\""), Some(XlValue::Bool(true)));
        assert_eq!(eval_one("=A1<>1"), Some(XlValue::Bool(false)));
        assert_eq!(eval_one("=\"a\">5"), Some(XlValue::Bool(true)));
        assert_eq!(eval_one("=TRUE"), Some(XlValue::Bool(true)));
        assert_eq!(eval_one("=#N/A"), Some(XlValue::Error(XlError::NA)));
    }

    #[test]
    fn aggregate_functions() {
        assert_eq!(num("=SUM(A1:A4)"), 10.0);
        assert_eq!(num("=SUM(A1:B4, 5)"), 15.0);
        assert_eq!(num("=SUM(A:A)"), 10.0);
        assert_eq!(num("=AVERAGE(A1:A4)"), 2.5);
        assert_eq!(num("=MIN(A1:A4)"), 1.0);
        assert_eq!(num("=MAX(A1:A4, 9)"), 9.0);
        assert_eq!(num("=COUNT(A1:B4)"), 4.0);
        assert_eq!(num("=COUNTA(A1:B4)"), 5.0);
        assert_eq!(num("=sum(a1:a2)"), 3.0);
        assert_eq!(
            eval_one("=AVERAGE(C1:C3)"),
            Some(XlValue::Error(XlError::Div0))
        );
    }

    #[test]
    fn logical_and_rounding_functions() {
        assert_eq!(num("=IF(A1>0, 10, 20)"), 10.0);
        assert_eq!(eval_one("=IF(A1>5, 10)"), Some(XlValue::Bool(false)));
        assert_eq!(num("=IF(A1>0, 1, 1/0)"), 1.0);
        assert_eq!(num("=IFERROR(1/0, 7)"), 7.0);
        assert_eq!(num("=IFERROR(3, 7)"), 3.0);
        assert_eq!(num("=ROUND(2.675, 2)"), 2.68);
        assert_eq!(num("=ROUND(-2.5, 0)"), -3.0);
        assert_eq!(num("=ROUND(1250, -2)"), 1300.0);
        assert_eq!(num("=ABS(-4.5)"), 4.5);
    }

    #[test]
    fn lookup_and_conditional_functions() {
        let m = model(&[
            (
                "Data",
                &[
                    ("A1", "apple".into()),
                    ("B1", 10.into()),
                    ("A2", "banana".into()),
                    ("B2", 20.into()),
                    ("A3", "apricot".into()),
                    ("B3", 30.into()),
                    ("D1", 1.into()),
                    ("E1", "low".into()),
                    ("D2", 100.into()),
                    ("E2", "high".into()),
                ],
            ),
            (
                "My Sheet",
                &[
                    (
                        "A1",
                        "=VLOOKUP(\"BANANA\", Data!$A$1:$B$3, 2, FALSE)".into(),
                    ),
                    ("A2", "=VLOOKUP(\"cherry\", Data!A1:B3, 2, FALSE)".into()),
                    ("A3", "=VLOOKUP(50, Data!D1:E2, 2)".into()),
                    ("A4", "=VLOOKUP(\"a*\", Data!A1:B3, 2, 0)".into()),
                    ("A5", "=SUMIF(Data!A1:A3, \"ap*\", Data!B1:B3)".into()),
                    ("A6", "=COUNTIF(Data!B1:B3, \">=20\")".into()),
                    ("A7", "=SUMIF(Data!B1:B3, \"<>20\")".into()),
                    ("A8", "=COUNTIF(Data!A1:A3, \"banana\")".into()),
                    ("A9", "=VLOOKUP(\"apple\", Data!A1:B3, 3, FALSE)".into()),
                    ("A10", "='My Sheet'!A1*2".into()),
                ],
            ),
        ]);
        let e = evaluate(&m);
        let at = |r: u32| e.result(1, r, 0).cloned();
        assert_eq!(at(0), Some(XlValue::Number(20.0)));
        assert_eq!(at(1), Some(XlValue::Error(XlError::NA)));
        assert_eq!(at(2), Some(XlValue::Text("low".into())));
        assert_eq!(at(3), Some(XlValue::Number(10.0)));
        assert_eq!(at(4), Some(XlValue::Number(40.0)));
        assert_eq!(at(5), Some(XlValue::Number(2.0)));
        assert_eq!(at(6), Some(XlValue::Number(40.0)));
        assert_eq!(at(7), Some(XlValue::Number(1.0)));
        assert_eq!(at(8), Some(XlValue::Error(XlError::Ref)));
        assert_eq!(at(9), Some(XlValue::Number(40.0)));
        assert!(e.summary().is_none());
    }

    #[test]
    fn dependency_order_across_sheets() {
        // Defined in the "wrong" order: C1 reads B1 reads A1 on another sheet.
        let m = model(&[
            ("Out", &[("C1", "=B1*2".into()), ("B1", "=In!A1+1".into())]),
            ("In", &[("A1", "=5*2".into())]),
        ]);
        let e = evaluate(&m);
        assert_eq!(e.result(0, 0, 2), Some(&XlValue::Number(22.0)));
    }

    #[test]
    fn unsupported_and_malformed_formulas_are_not_evaluated() {
        for f in [
            "=XLOOKUP(1, A1:A4, A1:A4)",
            "=SUM(A1:A4",
            "=A1+",
            "=MyName*2",
            "=Nowhere!A1",
            "=A1:A4",
            "=SUM(1,2)))",
            "=\"open",
            "=ROUND(1)",
            "=+",
            "=",
            "=A1 B1",
            "=Sheet1:Sheet3!A1",
            "=IF(",
            "=)(",
            "=XFE1",
            "=A1048577",
            "=日本",
        ] {
            assert_eq!(eval_one(f), None, "{f} should not evaluate");
        }
        let deep = format!("={}1{}", "(".repeat(500), ")".repeat(500));
        assert_eq!(eval_one(&deep), None);
        let neg = format!("={}1", "-".repeat(5000));
        assert_eq!(eval_one(&neg), None);
        assert_eq!(eval_one(&format!("=\"{}\"", "x".repeat(10_000))), None);
    }

    #[test]
    fn long_operator_chains_are_capped() {
        assert_eq!(num(&format!("={}1", "A1+".repeat(250))), 251.0);
        assert_eq!(eval_one(&format!("={}1", "A1+".repeat(251))), None);
        // Review item 6: the longest allowed chain fits a 512 KiB stack
        // (tokio's workers have 2 MiB) in a debug build.
        let chain = format!("={}1", "A1+".repeat(250));
        let worst = format!("=SUM({}1)", "-".repeat(60));
        let handle = std::thread::Builder::new()
            .stack_size(512 * 1024)
            .spawn(move || (eval_one(&chain), eval_one(&worst)))
            .unwrap();
        let (chain, worst) = handle.join().expect("no stack overflow");
        assert_eq!(chain, Some(XlValue::Number(251.0)));
        assert_eq!(worst, Some(XlValue::Number(1.0)));
    }

    #[test]
    fn cycles_and_their_dependents_are_listed() {
        let m = model(&[(
            "S",
            &[
                ("A1", "=B1+1".into()),
                ("B1", "=A1+1".into()),
                ("C1", "=A1*2".into()),
                ("D1", "=D1".into()),
                ("E1", "=SUM(E2:E9)+1".into()),
                ("E5", "=E1".into()),
                ("F1", "=1+1".into()),
            ],
        )]);
        let e = evaluate(&m);
        assert_eq!(
            e.unevaluated_cells(),
            vec!["S!A1", "S!B1", "S!C1", "S!D1", "S!E1", "S!E5"]
        );
        assert_eq!(e.result(0, 0, 5), Some(&XlValue::Number(2.0)));
        assert!(
            e.summary()
                .unwrap()
                .starts_with("6 formula cell(s) could not be precomputed: S!A1, S!B1")
        );
    }

    #[test]
    fn dependents_of_an_unevaluated_cell_are_not_evaluated() {
        let m = model(&[(
            "My Sheet",
            &[
                ("A1", "=XLOOKUP(1,B1:B2,C1:C2)".into()),
                ("A2", "=A1+1".into()),
            ],
        )]);
        let e = evaluate(&m);
        assert_eq!(
            e.unevaluated_cells(),
            vec!["'My Sheet'!A1", "'My Sheet'!A2"]
        );
    }

    #[test]
    fn summary_lists_at_most_twenty_cells() {
        let cells: Vec<(String, serde_json::Value)> = (1..=25)
            .map(|r| (format!("A{r}"), "=NOPE()".into()))
            .collect();
        let refs: Vec<(&str, serde_json::Value)> =
            cells.iter().map(|(r, v)| (r.as_str(), v.clone())).collect();
        let e = evaluate(&model(&[("S", &refs)]));
        let s = e.summary().unwrap();
        assert!(s.starts_with("25 formula cell(s)"));
        assert!(s.ends_with(", and 5 more"), "{s}");
    }

    #[test]
    fn cached_result_strings() {
        assert_eq!(XlValue::Number(1043750.0).to_cached_result(), "1043750");
        assert_eq!(XlValue::Number(-0.0).to_cached_result(), "0");
        assert_eq!(XlValue::Number(0.25).to_cached_result(), "0.25");
        assert_eq!(XlValue::Bool(true).to_cached_result(), "TRUE");
        assert_eq!(XlValue::Error(XlError::Div0).to_cached_result(), "#DIV/0!");
        assert_eq!(XlValue::Text("hi".into()).to_cached_result(), "hi");
    }

    #[test]
    fn wildcards() {
        let m = |p: &str, t: &str| wildcard_match(p, t).ok();
        assert_eq!(m("a*", "Apple"), Some(true));
        assert_eq!(m("a?ple", "apple"), Some(true));
        assert_eq!(m("*an*", "banana"), Some(true));
        assert_eq!(m("a?", "apple"), Some(false));
        assert_eq!(m("~*", "*"), Some(true));
        assert_eq!(m("~*", "x"), Some(false));
        assert_eq!(m("", ""), Some(true));
        // Excel folds non-ASCII case too: a miss there is not guessed.
        assert_eq!(m("é", "É"), None);
        assert_eq!(m("é", "é"), Some(true));
    }

    /// Review item 1: a value at S1048576 and `SUM(A:S)` used to build all
    /// 19.9M cells of the bounding box (+657 MB, 4.5 s).
    #[test]
    fn tall_sparse_sheet_is_read_by_its_cells_not_its_bounding_box() {
        let m = model(&[(
            "S",
            &[
                ("A1", 1.into()),
                ("S1048576", 2.into()),
                ("T1", "=SUM(A:S)".into()),
                ("T2", "=COUNTA(A:S)".into()),
                ("T3", "=COUNTIF(A:S,\"\")".into()),
                ("T4", "=SUMIF(A:S,\">0\")".into()),
                ("T5", "=COUNTIF(A1:B2,\"<>1\")".into()),
            ],
        )]);
        let started = std::time::Instant::now();
        let e = evaluate(&m);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(e.result(0, 0, 19), Some(&XlValue::Number(3.0)));
        assert_eq!(e.result(0, 1, 19), Some(&XlValue::Number(2.0)));
        assert_eq!(
            e.result(0, 2, 19),
            Some(&XlValue::Number(1_048_576.0 * 19.0 - 2.0))
        );
        assert_eq!(e.result(0, 3, 19), Some(&XlValue::Number(3.0)));
        assert_eq!(e.result(0, 4, 19), Some(&XlValue::Number(3.0)));
    }

    /// Review item 2: text Excel may coerce to a number is not guessed;
    /// text with no digit is still `#VALUE!`.
    #[test]
    fn text_that_excel_may_read_as_a_number_is_not_evaluated() {
        for f in [
            "=\"1,000\"+0",
            "=\"$5\"+1",
            "=\"50%\"+0",
            "=\"1/2\"+0",
            "=\"2024-01-05\"+0",
            "=SUM(\"1,000\")",
            "=COUNT(\"$5\")",
            "=COUNTIF(A1:A4,\">1,000\")",
        ] {
            assert_eq!(eval_one(f), None, "{f}");
        }
        assert_eq!(eval_one("=\"x\"+1"), Some(XlValue::Error(XlError::Value)));
        assert_eq!(num("=\" 12 \"+1"), 13.0);
    }

    /// Review item 3: results that hinge on binary noise are not guessed.
    #[test]
    fn float_noise_is_not_guessed() {
        assert_eq!(eval_one("=0.1+0.2=0.3"), None);
        assert_eq!(eval_one("=0.1+0.2<>0.3"), None);
        assert_eq!(eval_one("=0.5-0.4-0.1"), None);
        assert_eq!(eval_one("=SUM(0.1,0.2,-0.3)"), None);
        assert_eq!(num("=1-1"), 0.0);
        assert_eq!(num("=0.5-0.25"), 0.25);
        assert_eq!(eval_one("=0.5=0.5"), Some(XlValue::Bool(true)));
    }

    /// Review item 4: approximate VLOOKUP only over sorted, filled number
    /// keys; text order is not guessed.
    #[test]
    fn approximate_lookup_and_text_order_are_not_guessed() {
        let keys = |a: i64, b: i64, c: i64| -> Vec<(&'static str, serde_json::Value)> {
            vec![
                ("A1", a.into()),
                ("B1", "a".into()),
                ("A2", b.into()),
                ("B2", "b".into()),
                ("A3", c.into()),
                ("B3", "c".into()),
            ]
        };
        let run = |cells: Vec<(&str, serde_json::Value)>, f: &str| {
            let mut cells = cells;
            cells.push(("Z1", f.into()));
            evaluate(&model(&[("S", &cells)])).result(0, 0, 25).cloned()
        };
        assert_eq!(run(keys(30, 10, 20), "=VLOOKUP(10,A1:B3,2)"), None);
        assert_eq!(
            run(keys(10, 20, 30), "=VLOOKUP(25,A1:B3,2)"),
            Some(XlValue::Text("b".into()))
        );
        assert_eq!(
            run(keys(10, 20, 30), "=VLOOKUP(5,A1:B3,2)"),
            Some(XlValue::Error(XlError::NA))
        );
        // Blank rows below the keys, or a text lookup: not guessed.
        assert_eq!(run(keys(10, 20, 30), "=VLOOKUP(25,A1:B9,2)"), None);
        assert_eq!(run(keys(10, 20, 30), "=VLOOKUP(\"b\",B1:B3,1)"), None);
        // Exact match is unaffected by order.
        assert_eq!(
            run(keys(30, 10, 20), "=VLOOKUP(10,A1:B3,2,FALSE)"),
            Some(XlValue::Text("b".into()))
        );
        assert_eq!(eval_one("=\"a\"<\"b\""), None);
        assert_eq!(eval_one("=COUNTIF(B1:B1,\">a\")"), None);
        assert_eq!(eval_one("=\"a\"=\"A\""), Some(XlValue::Bool(true)));
        assert_eq!(eval_one("=\"\"<\"a\""), Some(XlValue::Bool(true)));
    }

    /// Review item 5: General format switches to scientific notation.
    #[test]
    fn concatenating_extreme_numbers_is_not_evaluated() {
        assert_eq!(eval_one("=0.0000001&\"\""), None);
        assert_eq!(eval_one("=1E15&\"\""), None);
        assert_eq!(eval_one("=123.5&\"\""), Some(XlValue::Text("123.5".into())));
        assert_eq!(
            eval_one("=0.0001&\"\""),
            Some(XlValue::Text("0.0001".into()))
        );
    }

    /// Review item 7: rust_xlsxwriter would store these texts as a number,
    /// boolean, error or 0.
    #[test]
    fn text_results_that_look_typed_are_demoted() {
        let m = model(&[(
            "S",
            &[
                ("A1", "=\"12\"&\"3\"".into()),
                ("A2", "=\"TRUE\"".into()),
                ("A3", "=\"#N/A\"".into()),
                ("A4", "=\"\"".into()),
                ("A5", "=\"ok\"".into()),
            ],
        )]);
        let mut e = evaluate(&m);
        e.demote_unstorable_text();
        assert_eq!(e.unevaluated_cells(), vec!["S!A1", "S!A2", "S!A3", "S!A4"]);
        assert_eq!(e.result(0, 4, 0), Some(&XlValue::Text("ok".into())));
    }
}
