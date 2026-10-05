//! Formula AutoComplete (#686, FRM-151, FRM-153, TBL-059): what the list
//! under a formula being typed offers at the caret.
//!
//! After `=`, an operator, `(`, `,` or a space, typed letters list the
//! functions ([`crate::formula::FUNCTION_NAMES`]), defined names and table
//! names that begin with them. After `Table[` the list is the table's
//! columns, then `#All`, `#Data`, `#Headers`, `#Totals` and `@ - This Row`.
//! Nothing is offered outside a formula or inside a string literal. The host
//! shows the list, moves its highlight, and on Tab replaces the chars
//! `start..caret` of the buffer with the chosen [`Item::insert`].

use crate::formula::FUNCTION_NAMES;
use crate::sheet::Workbook;

/// What an item is, for its icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Function,
    Name,
    Table,
    Column,
    Specifier,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Function => "function",
            Kind::Name => "name",
            Kind::Table => "table",
            Kind::Column => "column",
            Kind::Specifier => "specifier",
        }
    }
}

/// One entry of the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    /// What the list shows.
    pub label: String,
    /// What Tab puts in place of the typed prefix: a function with its `(`,
    /// a column escaped for a structured reference, `@` for `@ - This Row`.
    pub insert: String,
    pub kind: Kind,
}

/// The list at a caret: its items, in order, and the char range of the
/// buffer an inserted item replaces (`start..caret`, the typed prefix).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completions {
    pub start: usize,
    pub items: Vec<Item>,
}

impl Completions {
    /// `buf` with item `i` inserted for the prefix ending at `caret`, and the
    /// caret after it.
    pub fn insert(&self, buf: &str, caret: usize, i: usize) -> Option<(String, usize)> {
        let item = self.items.get(i)?;
        let mut out: String = buf.chars().take(self.start).collect();
        out.push_str(&item.insert);
        let at = out.chars().count();
        out.extend(buf.chars().skip(caret));
        Some((out, at))
    }
}

const SPECIFIERS: [&str; 4] = ["#All", "#Data", "#Headers", "#Totals"];
const THIS_ROW: &str = "@ - This Row";

/// The completions for the formula `buf` (with its leading `=`) at char
/// `caret`, on sheet `sheet` of `wb`; `None` when there is no list. With
/// `demand` (Alt+Down), an empty prefix at a place a name can go lists
/// everything; typing alone needs a letter.
pub fn completions(
    buf: &str,
    caret: usize,
    wb: &Workbook,
    sheet: usize,
    demand: bool,
) -> Option<Completions> {
    let chars: Vec<char> = buf.chars().collect();
    if chars.first() != Some(&'=') || caret == 0 || caret > chars.len() {
        return None;
    }
    let before = &chars[..caret];
    let scan = scan(before)?;
    if let Some(open) = scan.innermost_bracket {
        return in_brackets(before, open, &scan, wb);
    }
    // The identifier being typed.
    let mut start = caret;
    while start > 1 && is_ident(before[start - 1]) {
        start -= 1;
    }
    let prefix: String = before[start..].iter().collect();
    if prefix.is_empty() && !demand {
        return None;
    }
    if prefix
        .chars()
        .next()
        .is_some_and(|c| !(c.is_alphabetic() || c == '_' || c == '\\'))
    {
        return None;
    }
    // What comes before it must be where a name can start.
    let prev = before[..start]
        .iter()
        .rev()
        .find(|c| !c.is_whitespace())
        .copied();
    let spaced = start > 0 && before[start - 1].is_whitespace();
    let ok = match prev {
        Some('=' | '+' | '-' | '*' | '/' | '^' | '&' | '<' | '>' | '(' | ',' | ';' | '{' | '%') => {
            true
        }
        // `A1 B` (the intersection operator) or a name after a closing
        // parenthesis and a space.
        Some(_) => spaced && prev != Some(':') && prev != Some('!'),
        None => false,
    };
    if !ok {
        return None;
    }
    let items = names(&prefix, wb, sheet);
    (!items.is_empty()).then_some(Completions { start, items })
}

/// The identifier chars of a name or function: letters, digits, `_`, `.`
/// and `\`.
fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '.' | '\\')
}

/// Where the caret is in the formula's lexical structure.
struct Scan {
    /// The char index of the innermost unclosed `[`, if any.
    innermost_bracket: Option<usize>,
    /// The char index of the outermost unclosed `[`, if any.
    outer_bracket: Option<usize>,
}

/// Walk the formula up to the caret: `None` inside a string literal or a
/// quoted sheet name, where nothing is offered.
fn scan(before: &[char]) -> Option<Scan> {
    let mut in_string = false;
    let mut in_quote = false;
    let mut open: Vec<usize> = Vec::new();
    let mut i = 0;
    while i < before.len() {
        let c = before[i];
        if in_string {
            if c == '"' {
                in_string = false;
            }
        } else if in_quote {
            if c == '\'' {
                in_quote = false;
            }
        } else if !open.is_empty() {
            match c {
                '\'' => i += 1, // escapes the next char
                '[' => open.push(i),
                ']' => {
                    open.pop();
                }
                _ => {}
            }
        } else {
            match c {
                '"' => in_string = true,
                '\'' => in_quote = true,
                '[' => open.push(i),
                _ => {}
            }
        }
        i += 1;
    }
    if in_string || in_quote {
        return None;
    }
    Some(Scan {
        innermost_bracket: open.last().copied(),
        outer_bracket: open.first().copied(),
    })
}

/// The list inside `Table[…`: the table's columns, then the specifiers,
/// those starting with the typed prefix.
fn in_brackets(before: &[char], open: usize, scan: &Scan, wb: &Workbook) -> Option<Completions> {
    let outer = scan.outer_bracket?;
    // The table name before the outer `[`.
    let mut s = outer;
    while s > 0 && is_ident(before[s - 1]) {
        s -= 1;
    }
    let name: String = before[s..outer].iter().collect();
    let table = wb.table(&name)?;
    // Inside `Table[`, or in a nested `[` right after `Table[` or a comma.
    if open != outer && !matches!(before[open - 1], '[' | ',' | ' ' | ':') {
        return None;
    }
    let typed = crate::formula::unescape_spec(&before[open + 1..].iter().collect::<String>());
    let lower = typed.to_lowercase();
    let matches = |label: &str| label.to_lowercase().starts_with(&lower);
    let mut items: Vec<Item> = table
        .columns
        .iter()
        .filter(|c| matches(c))
        .map(|c| Item {
            label: c.clone(),
            insert: crate::formula::escape_spec(c),
            kind: Kind::Column,
        })
        .collect();
    items.extend(SPECIFIERS.iter().filter(|s| matches(s)).map(|s| Item {
        label: s.to_string(),
        insert: s.to_string(),
        kind: Kind::Specifier,
    }));
    if typed.is_empty() || typed == "@" {
        items.push(Item {
            label: THIS_ROW.into(),
            insert: "@".into(),
            kind: Kind::Specifier,
        });
    }
    (!items.is_empty()).then_some(Completions {
        start: open + 1,
        items,
    })
}

/// The functions, defined names (global, or scoped to `sheet`) and tables
/// starting with `prefix`, sorted ignoring case.
fn names(prefix: &str, wb: &Workbook, sheet: usize) -> Vec<Item> {
    let lower = prefix.to_lowercase();
    let starts = |s: &str| s.to_lowercase().starts_with(&lower);
    let mut items: Vec<Item> = FUNCTION_NAMES
        .iter()
        .filter(|f| starts(f))
        .map(|f| Item {
            label: f.to_string(),
            insert: format!("{f}("),
            kind: Kind::Function,
        })
        .collect();
    for d in &wb.defined_names {
        // Excel lists none of the `_xl…` names: the `_xlnm.` built-ins
        // (Print_Area, _FilterDatabase) and the `_xlfn.`/`_xlpm.` spellings.
        if d.name.starts_with("_xl") || !d.scope.is_none_or(|s| s == sheet) || !starts(&d.name) {
            continue;
        }
        if items.iter().any(|i| i.label.eq_ignore_ascii_case(&d.name)) {
            continue;
        }
        items.push(Item {
            label: d.name.clone(),
            insert: d.name.clone(),
            kind: Kind::Name,
        });
    }
    for t in &wb.tables {
        if starts(&t.name) && !items.iter().any(|i| i.label.eq_ignore_ascii_case(&t.name)) {
            items.push(Item {
                label: t.name.clone(),
                insert: t.name.clone(),
                kind: Kind::Table,
            });
        }
    }
    items.sort_by(|a, b| {
        a.label
            .to_lowercase()
            .cmp(&b.label.to_lowercase())
            .then_with(|| a.label.cmp(&b.label))
    });
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sheet::{DefinedName, Sheet, Table};

    fn sales() -> Workbook {
        Workbook {
            sheets: vec![Sheet::default(), Sheet::default()],
            tables: vec![Table {
                name: "Sales".into(),
                sheet: 0,
                range: (0, 0, 4, 3),
                header_rows: 1,
                totals_rows: 0,
                columns: vec!["Item".into(), "Qty".into(), "Price".into(), "Region".into()],
                column_ids: Vec::new(),
                part: String::new(),
            }],
            defined_names: vec![
                DefinedName {
                    name: "SalesTax".into(),
                    scope: None,
                    formula: "0.2".into(),
                },
                DefinedName {
                    name: "SumLocal".into(),
                    scope: Some(1),
                    formula: "1".into(),
                },
                DefinedName {
                    name: "_xlnm.Print_Area".into(),
                    scope: None,
                    formula: "Sheet1!$A$1".into(),
                },
            ],
            ..Workbook::default()
        }
    }

    fn labels(buf: &str) -> Vec<String> {
        labels_on(buf, 0)
    }

    fn labels_on(buf: &str, sheet: usize) -> Vec<String> {
        completions(buf, buf.chars().count(), &sales(), sheet, false)
            .map(|c| c.items.into_iter().map(|i| i.label).collect())
            .unwrap_or_default()
    }

    #[test]
    fn tbl_case_021_table_names_then_columns_and_specifiers() {
        assert_eq!(labels("=SUM(Sal"), ["Sales", "SalesTax"]);
        assert_eq!(
            labels("=SUM(Sales["),
            [
                "Item",
                "Qty",
                "Price",
                "Region",
                "#All",
                "#Data",
                "#Headers",
                "#Totals",
                "@ - This Row"
            ]
        );
        assert_eq!(labels("=SUM(Sales[q"), ["Qty"]);
        assert_eq!(labels("=SUM(Sales[#t"), ["#Totals"]);
        assert_eq!(labels("=SUM(Sales[@"), ["@ - This Row"]);
        assert_eq!(labels("=SUM(Sales[[#All],["), {
            let mut v = vec!["Item", "Qty", "Price", "Region"];
            v.extend(SPECIFIERS);
            v.push(THIS_ROW);
            v
        });
        // A closed bracket is done; an unknown table has no list.
        assert!(labels("=SUM(Sales[Qty]").is_empty());
        assert!(labels("=SUM(Nope[").is_empty());
    }

    #[test]
    fn functions_names_and_tables_sorted() {
        let su = labels("=SU");
        assert_eq!(
            su[..7],
            [
                "SUBSTITUTE",
                "SUBTOTAL",
                "SUM",
                "SUMIF",
                "SUMIFS",
                "SUMPRODUCT",
                "SUMSQ"
            ]
        );
        assert!(!su.contains(&"SumLocal".to_string()), "scoped to sheet 2");
        assert!(labels_on("=SU", 1).contains(&"SumLocal".to_string()));
        assert!(labels("=_x").is_empty(), "hidden names are not offered");
        assert_eq!(labels("=A")[..2], ["ABS", "ACOS"]);
        // After operators, commas, parentheses and spaces.
        for buf in ["=1+su", "=IF(A1,su", "=(su", "=A1:A2 su", "=x&su"] {
            assert!(labels(buf).contains(&"SUM".to_string()), "{buf}");
        }
    }

    #[test]
    fn nothing_outside_a_formula_in_a_string_or_after_a_reference() {
        for buf in [
            "su",
            "=\"su",
            "=\"a\"&\"su",
            "='My Sheet",
            "=A1",
            "=1su",
            "=Sheet1!su",
            "=A1:su",
            "=SUM(A1)su",
            "=su(",
            "=qqq",
        ] {
            assert!(labels(buf).is_empty(), "{buf}");
        }
        // A closed string is not a string any more.
        assert!(labels("=\"a\"&su").contains(&"SUM".to_string()));
        // `=` alone lists nothing while typing; Alt+Down lists everything.
        assert!(labels("=").is_empty());
        let all = completions("=", 1, &sales(), 0, true).unwrap();
        assert!(all.items.len() > 300);
        assert!(completions("=SUM(A1", 7, &sales(), 0, true).is_none());
    }

    #[test]
    fn tab_inserts_the_item() {
        let wb = sales();
        let c = completions("=su", 3, &wb, 0, false).unwrap();
        let sum = c.items.iter().position(|i| i.label == "SUM").unwrap();
        assert_eq!(c.insert("=su", 3, sum), Some(("=SUM(".into(), 5)));
        let buf = "=SUM(Sales[";
        let c = completions(buf, 11, &wb, 0, false).unwrap();
        assert_eq!(c.insert(buf, 11, 1), Some(("=SUM(Sales[Qty".into(), 14)));
        assert_eq!(c.insert(buf, 11, 8), Some(("=SUM(Sales[@".into(), 12)));
        // In the middle of a formula, the rest stays after the insert.
        let buf = "=Sal+1";
        let c = completions(buf, 4, &wb, 0, false).unwrap();
        assert_eq!(c.insert(buf, 4, 0), Some(("=Sales+1".into(), 6)));
    }

    #[test]
    fn special_columns_are_escaped_and_parse_back() {
        use crate::formula::escape_spec;
        assert_eq!(escape_spec("#Units"), "'#Units");
        assert_eq!(escape_spec("a[b]'c@"), "a'[b']''c'@");
        let mut wb = sales();
        wb.tables[0].columns[1] = "#Units".into();
        let buf = "=SUM(Sales['#";
        let c = completions(buf, buf.chars().count(), &wb, 0, false).unwrap();
        assert_eq!(c.items[0].label, "#Units");
        let (text, _) = c.insert(buf, buf.chars().count(), 0).unwrap();
        assert_eq!(text, "=SUM(Sales['#Units");
        let parsed = crate::formula::parse(&format!("{}])", &text[1..])).unwrap();
        let crate::formula::Expr::Func(_, args) = parsed else {
            panic!("{parsed:?}")
        };
        assert!(
            matches!(&args[0], crate::formula::Expr::Structured { col1: Some(c), .. } if c == "#Units"),
            "{args:?}"
        );
    }
}
