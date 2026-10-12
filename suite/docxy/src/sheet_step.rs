//! What a sheet undo step is called (#1149): the name Excel's Quick Access
//! Toolbar Undo and Redo tips and drop-downs give it (`Undo Typing 'abc' in
//! A1 (Ctrl+Z)`, `Redo Fill (Ctrl+Y)`). Every push onto a sheet's undo stack
//! takes one, so no step goes unnamed.

use gridcore::sheet::cell_name;

/// Typed text longer than this many characters is shortened in the name, with
/// `...` after it: Excel shortens long entries in its list (the length is not
/// measured).
const TYPED_SHOWN: usize = 40;

/// One undo step's name.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SheetStep {
    /// An entry typed into a cell (`at` is `A1`) or, by Ctrl+Enter, into a
    /// range (`A1:B3`, or every area of the selection: `A1:A3,C5`): `text`
    /// is what lands in the cell, after AutoComplete and AutoCorrect.
    Typing { text: String, at: String },
    /// A command, by Excel's name for it (`Fill`, `Paste`, `Clear`, ...).
    Named(&'static str),
}

impl SheetStep {
    /// Typing `text` into the cell at 0-based `(r, c)`.
    pub(crate) fn typing(text: &str, (r, c): (u32, u32)) -> Self {
        Self::Typing {
            text: text.to_string(),
            at: cell_name(r, c),
        }
    }

    /// Typing `text` into every 0-based area `(r0, c0, r1, c1)` of a
    /// selection, in selection order (Ctrl+Enter, #670): `A1:A3,C5`; a
    /// one-cell area reads as `A1`.
    pub(crate) fn typing_areas(text: &str, areas: &[(u32, u32, u32, u32)]) -> Self {
        let at = areas
            .iter()
            .map(|&(r0, c0, r1, c1)| {
                if (r0, c0) == (r1, c1) {
                    cell_name(r0, c0)
                } else {
                    format!("{}:{}", cell_name(r0, c0), cell_name(r1, c1))
                }
            })
            .collect::<Vec<_>>()
            .join(",");
        Self::Typing {
            text: text.to_string(),
            at,
        }
    }

    /// The name as the tips and the drop-down list show it.
    pub(crate) fn label(&self) -> String {
        match self {
            Self::Typing { text, at } => {
                let shown: String = if text.chars().count() > TYPED_SHOWN {
                    let mut s: String = text.chars().take(TYPED_SHOWN).collect();
                    s.push_str("...");
                    s
                } else {
                    text.clone()
                };
                format!("Typing '{shown}' in {at}")
            }
            Self::Named(name) => (*name).to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_read_as_excel_names_them_1149() {
        assert_eq!(
            SheetStep::typing("abc", (0, 0)).label(),
            "Typing 'abc' in A1"
        );
        assert_eq!(SheetStep::typing("", (2, 27)).label(), "Typing '' in AB3");
        assert_eq!(
            SheetStep::typing_areas("x", &[(0, 0, 2, 1)]).label(),
            "Typing 'x' in A1:B3"
        );
        assert_eq!(
            SheetStep::typing_areas("x", &[(4, 2, 4, 2)]).label(),
            "Typing 'x' in C5"
        );
        assert_eq!(
            SheetStep::typing_areas("x", &[(0, 0, 2, 0), (4, 2, 4, 2)]).label(),
            "Typing 'x' in A1:A3,C5"
        );
        assert_eq!(SheetStep::Named("Fill").label(), "Fill");
    }

    #[test]
    fn long_typed_text_is_shortened_1149() {
        let long = "é".repeat(TYPED_SHOWN + 1);
        let label = SheetStep::typing(&long, (0, 0)).label();
        assert_eq!(
            label,
            format!("Typing '{}...' in A1", "é".repeat(TYPED_SHOWN))
        );
        let exact = "a".repeat(TYPED_SHOWN);
        assert_eq!(
            SheetStep::typing(&exact, (0, 0)).label(),
            format!("Typing '{exact}' in A1")
        );
    }
}
