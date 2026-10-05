//! Excel's File › Options › Advanced › Editing options for sheets (#672): the
//! per-user switches every sheet host honours, and the `key=value` text both
//! hosts persist them as (xlsxy in `view.conf`, the suite in `session.json`).
//!
//! The rules the options drive live with the code they change: the fixed
//! decimal in [`crate::entry::EntryCtx::fixed_decimal`], AutoComplete in
//! [`crate::entry::autocomplete`], the precedents a double-click jumps to in
//! [`crate::formula::direct_precedents`], the automatic Flash Fill preview in
//! [`crate::flashfill::flash_preview`], Formula AutoComplete in
//! [`crate::fcomplete::completions`]. The hosts read the switches here.

/// The direction "After pressing Enter, move selection" moves in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EnterMove {
    #[default]
    Down,
    Right,
    Up,
    Left,
}

impl EnterMove {
    /// Every direction, in the order Excel's list shows them.
    pub const ALL: [EnterMove; 4] = [
        EnterMove::Down,
        EnterMove::Right,
        EnterMove::Up,
        EnterMove::Left,
    ];

    /// The (row, col) step one Enter takes.
    pub fn delta(self) -> (i32, i32) {
        match self {
            EnterMove::Down => (1, 0),
            EnterMove::Right => (0, 1),
            EnterMove::Up => (-1, 0),
            EnterMove::Left => (0, -1),
        }
    }

    /// The name Excel's list shows (and the persisted value, lowercased).
    pub fn label(self) -> &'static str {
        match self {
            EnterMove::Down => "Down",
            EnterMove::Right => "Right",
            EnterMove::Up => "Up",
            EnterMove::Left => "Left",
        }
    }

    /// The direction a label or persisted value names, any case.
    pub fn from_label(s: &str) -> Option<EnterMove> {
        EnterMove::ALL
            .into_iter()
            .find(|m| m.label().eq_ignore_ascii_case(s.trim()))
    }
}

/// The places "Automatically insert a decimal point" accepts.
pub const PLACES_MIN: i16 = -300;
pub const PLACES_MAX: i16 = 300;

/// The Editing options. `Default` is Excel's: fixed decimal off with 2
/// places, Enter moves down, editing directly in cells, AutoComplete, the
/// fill handle, Automatically Flash Fill and Formula AutoComplete on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditOptions {
    /// Automatically insert a decimal point.
    pub fixed_decimal: bool,
    /// Its Places, `PLACES_MIN..=PLACES_MAX`; negative adds zeros.
    pub places: i16,
    /// After pressing Enter, move selection.
    pub move_after_enter: bool,
    /// Its Direction.
    pub enter_move: EnterMove,
    /// Allow editing directly in cells. Off, an editor's caret lives in the
    /// formula bar and a double-click on a formula jumps to its precedents.
    pub edit_in_cell: bool,
    /// Enable AutoComplete for cell values.
    pub autocomplete: bool,
    /// Enable fill handle and cell drag-and-drop.
    pub fill_handle: bool,
    /// Automatically Flash Fill (Advanced): the greyed preview after the
    /// second example is typed.
    pub flash_fill_auto: bool,
    /// Formula AutoComplete (Formulas › Working with formulas).
    pub formula_autocomplete: bool,
}

impl Default for EditOptions {
    fn default() -> Self {
        EditOptions {
            fixed_decimal: false,
            places: 2,
            move_after_enter: true,
            enter_move: EnterMove::Down,
            edit_in_cell: true,
            autocomplete: true,
            fill_handle: true,
            flash_fill_auto: true,
            formula_autocomplete: true,
        }
    }
}

/// The persisted keys. Prefixed, so they can share a file with a host's other
/// preferences.
pub const KEY_FIXED_DECIMAL: &str = "edit_fixed_decimal";
pub const KEY_PLACES: &str = "edit_fixed_decimal_places";
pub const KEY_MOVE_AFTER_ENTER: &str = "edit_move_after_enter";
pub const KEY_MOVE_DIRECTION: &str = "edit_move_direction";
pub const KEY_EDIT_IN_CELL: &str = "edit_in_cell";
pub const KEY_AUTOCOMPLETE: &str = "edit_autocomplete";
pub const KEY_FILL_HANDLE: &str = "edit_fill_handle";
pub const KEY_FLASH_FILL_AUTO: &str = "edit_flash_fill_auto";
pub const KEY_FORMULA_AUTOCOMPLETE: &str = "edit_formula_autocomplete";

impl EditOptions {
    /// The places a typed commit shifts by: `Some` only while the fixed
    /// decimal is on (what [`crate::entry::EntryCtx::fixed_decimal`] takes).
    pub fn fixed_places(&self) -> Option<i16> {
        self.fixed_decimal.then_some(self.places)
    }

    /// Where Enter (`back` = Shift+Enter, the opposite way) moves the
    /// selection: nowhere while "move selection" is off.
    pub fn enter_delta(&self, back: bool) -> (i32, i32) {
        if !self.move_after_enter {
            return (0, 0);
        }
        let (dr, dc) = self.enter_move.delta();
        if back { (-dr, -dc) } else { (dr, dc) }
    }

    /// The options a preferences text gives: every known key read, anything
    /// else (other preferences, unknown keys) ignored, and a missing or
    /// malformed value keeping Excel's default.
    pub fn from_text(text: &str) -> EditOptions {
        let mut o = EditOptions::default();
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let v = v.trim();
            let flag = |slot: &mut bool| {
                if let Some(b) = parse_bool(v) {
                    *slot = b;
                }
            };
            match k.trim() {
                KEY_FIXED_DECIMAL => flag(&mut o.fixed_decimal),
                KEY_PLACES => {
                    if let Some(p) = v
                        .parse::<i16>()
                        .ok()
                        .filter(|p| (PLACES_MIN..=PLACES_MAX).contains(p))
                    {
                        o.places = p;
                    }
                }
                KEY_MOVE_AFTER_ENTER => flag(&mut o.move_after_enter),
                KEY_MOVE_DIRECTION => {
                    if let Some(m) = EnterMove::from_label(v) {
                        o.enter_move = m;
                    }
                }
                KEY_EDIT_IN_CELL => flag(&mut o.edit_in_cell),
                KEY_AUTOCOMPLETE => flag(&mut o.autocomplete),
                KEY_FILL_HANDLE => flag(&mut o.fill_handle),
                KEY_FLASH_FILL_AUTO => flag(&mut o.flash_fill_auto),
                KEY_FORMULA_AUTOCOMPLETE => flag(&mut o.formula_autocomplete),
                _ => {}
            }
        }
        o
    }

    /// The options as `key=value` lines, each ending in `\n`; what
    /// [`EditOptions::from_text`] reads back.
    pub fn to_lines(&self) -> String {
        let b = |on: bool| u8::from(on);
        format!(
            "{KEY_FIXED_DECIMAL}={}\n{KEY_PLACES}={}\n{KEY_MOVE_AFTER_ENTER}={}\n\
             {KEY_MOVE_DIRECTION}={}\n{KEY_EDIT_IN_CELL}={}\n{KEY_AUTOCOMPLETE}={}\n\
             {KEY_FILL_HANDLE}={}\n{KEY_FLASH_FILL_AUTO}={}\n{KEY_FORMULA_AUTOCOMPLETE}={}\n",
            b(self.fixed_decimal),
            self.places,
            b(self.move_after_enter),
            self.enter_move.label().to_ascii_lowercase(),
            b(self.edit_in_cell),
            b(self.autocomplete),
            b(self.fill_handle),
            b(self.flash_fill_auto),
            b(self.formula_autocomplete),
        )
    }
}

fn parse_bool(v: &str) -> Option<bool> {
    match v {
        "1" => Some(true),
        "0" => Some(false),
        _ if v.eq_ignore_ascii_case("true") => Some(true),
        _ if v.eq_ignore_ascii_case("false") => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_excels() {
        let o = EditOptions::default();
        assert!(!o.fixed_decimal);
        assert_eq!(o.places, 2);
        assert!(o.move_after_enter);
        assert_eq!(o.enter_move, EnterMove::Down);
        assert!(o.edit_in_cell && o.autocomplete && o.fill_handle);
        assert!(o.flash_fill_auto && o.formula_autocomplete);
        assert_eq!(o.fixed_places(), None);
    }

    #[test]
    fn options_round_trip_through_their_text() {
        let o = EditOptions {
            fixed_decimal: true,
            places: -3,
            move_after_enter: false,
            enter_move: EnterMove::Left,
            edit_in_cell: false,
            autocomplete: false,
            fill_handle: false,
            flash_fill_auto: false,
            formula_autocomplete: false,
        };
        assert_eq!(EditOptions::from_text(&o.to_lines()), o);
        let d = EditOptions::default();
        assert_eq!(EditOptions::from_text(&d.to_lines()), d);
    }

    #[test]
    fn a_malformed_or_unknown_value_keeps_the_default() {
        let text = "formula_view=1\nedit_fixed_decimal=maybe\nedit_fixed_decimal_places=301\n\
                    edit_move_direction=sideways\nedit_autocomplete=0\nedit_in_cell=true\n\
                    no equals sign\nedit_unknown=1\n";
        let o = EditOptions::from_text(text);
        let d = EditOptions::default();
        assert_eq!(o.fixed_decimal, d.fixed_decimal);
        assert_eq!(o.places, d.places);
        assert_eq!(o.enter_move, d.enter_move);
        assert!(
            !o.autocomplete,
            "a well-formed key next to bad ones is still read"
        );
        assert!(o.edit_in_cell);
        assert_eq!(EditOptions::from_text(""), d);
        assert_eq!(
            EditOptions::from_text("edit_fixed_decimal_places=-300").places,
            -300
        );
    }

    #[test]
    fn text_saved_before_the_flash_fill_and_formula_switches_reads_them_on() {
        // A preferences text from before #712 has neither key.
        let old = "edit_fixed_decimal=1\nedit_autocomplete=0\nedit_fill_handle=1\n";
        let o = EditOptions::from_text(old);
        assert!(o.fixed_decimal && !o.autocomplete);
        assert!(o.flash_fill_auto && o.formula_autocomplete);
        let off =
            EditOptions::from_text("edit_flash_fill_auto=0\nedit_formula_autocomplete=false\n");
        assert!(!off.flash_fill_auto && !off.formula_autocomplete);
    }

    #[test]
    fn enter_moves_by_direction_and_shift_reverses_it() {
        assert_eq!(EnterMove::Down.delta(), (1, 0));
        assert_eq!(EnterMove::Right.delta(), (0, 1));
        assert_eq!(EnterMove::Up.delta(), (-1, 0));
        assert_eq!(EnterMove::Left.delta(), (0, -1));
        let mut o = EditOptions {
            enter_move: EnterMove::Right,
            ..EditOptions::default()
        };
        assert_eq!(o.enter_delta(false), (0, 1));
        assert_eq!(o.enter_delta(true), (0, -1));
        o.move_after_enter = false;
        assert_eq!(o.enter_delta(false), (0, 0));
        assert_eq!(o.enter_delta(true), (0, 0));
        assert_eq!(EnterMove::from_label(" up "), Some(EnterMove::Up));
    }

    #[test]
    fn fixed_places_only_while_on() {
        let o = EditOptions {
            fixed_decimal: true,
            places: -2,
            ..EditOptions::default()
        };
        assert_eq!(o.fixed_places(), Some(-2));
    }
}
