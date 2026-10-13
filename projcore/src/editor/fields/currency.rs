//! Money text in the plan's currency (#579): the MSPDI `CurrencySymbol`,
//! `CurrencyDigits` and `CurrencySymbolPosition` options, kept as stored
//! project options and read here. A plan with none of them reads as Project's
//! US default, `$1,400.00`. The thousands and decimal separators stay `,` and
//! `.`: MSPDI does not carry them (Project takes them from the regional
//! settings).
use crate::model::Project;

/// Where the currency symbol goes, as MSPDI's `CurrencySymbolPosition`
/// codes it: 0 before (`$1`), 1 after (`1$`), 2 before with a space
/// (`$ 1`), 3 after with a space (`1 $`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SymbolPosition {
    #[default]
    Before,
    After,
    BeforeSpaced,
    AfterSpaced,
}

impl SymbolPosition {
    pub fn from_code(code: i64) -> Option<Self> {
        Some(match code {
            0 => Self::Before,
            1 => Self::After,
            2 => Self::BeforeSpaced,
            3 => Self::AfterSpaced,
            _ => return None,
        })
    }
}

/// How a plan shows money.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Currency {
    /// The symbol as stored; empty shows a bare number, as Project does,
    /// rather than inventing a currency the plan did not state.
    pub symbol: String,
    /// Decimal digits, 0..=4.
    pub digits: u8,
    pub position: SymbolPosition,
}

impl Default for Currency {
    /// Project's US default: `$`, 2 digits, symbol before.
    fn default() -> Self {
        Currency {
            symbol: "$".into(),
            digits: 2,
            position: SymbolPosition::Before,
        }
    }
}

impl Currency {
    /// The plan's currency. Lenient, never an error: an absent symbol is
    /// `$`, an unparsable or out-of-range digit count is 2, an unknown
    /// position is before.
    pub fn of(proj: &Project) -> Currency {
        let default = Currency::default();
        let code = |name: &str| proj.option(name).and_then(|v| v.trim().parse::<i64>().ok());
        Currency {
            symbol: proj
                .option("CurrencySymbol")
                .map_or(default.symbol, str::to_string),
            digits: code("CurrencyDigits")
                .filter(|d| (0..=4).contains(d))
                .map_or(default.digits, |d| d as u8),
            position: code("CurrencySymbolPosition")
                .and_then(SymbolPosition::from_code)
                .unwrap_or(default.position),
        }
    }

    /// Money as Project shows it: rounded to the plan's digits, thousands
    /// grouped, the symbol placed by position, a negative in parentheses
    /// around the whole (`$1,400.00`, `($40,000.00)`, `1,400.00 €`).
    pub fn format(&self, units: f64) -> String {
        let scale = 10i128.pow(u32::from(self.digits));
        let scaled = (units * scale as f64).round() as i128;
        let abs = scaled.unsigned_abs();
        let whole = (abs / scale.unsigned_abs()).to_string();
        let mut number = String::new();
        for (i, c) in whole.chars().enumerate() {
            if i > 0 && (whole.len() - i).is_multiple_of(3) {
                number.push(',');
            }
            number.push(c);
        }
        if self.digits > 0 {
            let frac = abs % scale.unsigned_abs();
            number.push_str(&format!(
                ".{frac:0width$}",
                width = usize::from(self.digits)
            ));
        }
        let symbol = self.symbol.as_str();
        let shown = match (symbol.is_empty(), self.position) {
            (true, _) => number,
            (false, SymbolPosition::Before) => format!("{symbol}{number}"),
            (false, SymbolPosition::After) => format!("{number}{symbol}"),
            (false, SymbolPosition::BeforeSpaced) => format!("{symbol} {number}"),
            (false, SymbolPosition::AfterSpaced) => format!("{number} {symbol}"),
        };
        if scaled < 0 {
            format!("({shown})")
        } else {
            shown
        }
    }
}
