//! Excel's File › Options › Advanced › Editing options for sheets (#672): the
//! per-user switches every sheet host honours, and the `key=value` text both
//! hosts persist them as (xlsxy in `view.conf`, the suite in `session.json`).
//!
//! The rules the options drive live with the code they change: the fixed
//! decimal in [`crate::entry::EntryCtx::fixed_decimal`], AutoComplete in
//! [`crate::entry::autocomplete`], the precedents a double-click jumps to in
//! [`crate::formula::direct_precedents`]. The hosts read the switches here.

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
/// places, Enter moves down, editing directly in cells, AutoComplete and the
/// fill handle on.
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
             {KEY_FILL_HANDLE}={}\n",
            b(self.fixed_decimal),
            self.places,
            b(self.move_after_enter),
            self.enter_move.label().to_ascii_lowercase(),
            b(self.edit_in_cell),
            b(self.autocomplete),
            b(self.fill_handle),
        )
    }
}

/// The persisted key of one custom list (File › Options › Advanced › Edit
/// Custom Lists, #668): one `edit_custom_list=a,b,c` line per list, a `,` or
/// `\` inside an item escaped with `\`.
pub const KEY_CUSTOM_LIST: &str = "edit_custom_list";

/// The user's custom lists in a preferences text, in order; empty lists and
/// items are dropped.
pub fn custom_lists_from_text(text: &str) -> Vec<Vec<String>> {
    text.lines()
        .filter_map(|line| {
            let (k, v) = line.split_once('=')?;
            (k.trim() == KEY_CUSTOM_LIST).then(|| split_list(v))
        })
        .filter(|l| !l.is_empty())
        .collect()
}

/// `lists` as `key=value` lines, each ending in `\n`; what
/// [`custom_lists_from_text`] reads back. A backslash, comma or line break
/// in an item is escaped, so an item imported from a cell with a line break
/// keeps its list on one line (#707 r6 m1).
pub fn custom_lists_to_lines(lists: &[Vec<String>]) -> String {
    let mut out = String::new();
    for list in lists.iter().filter(|l| !l.is_empty()) {
        let items: Vec<String> = list
            .iter()
            .map(|i| {
                i.replace('\\', "\\\\")
                    .replace(',', "\\,")
                    .replace('\n', "\\n")
                    .replace('\r', "\\r")
            })
            .collect();
        out.push_str(&format!("{KEY_CUSTOM_LIST}={}\n", items.join(",")));
    }
    out
}

fn split_list(v: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut cur = String::new();
    let mut chars = v.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => match chars.next() {
                Some('n') => cur.push('\n'),
                Some('r') => cur.push('\r'),
                next => cur.extend(next),
            },
            ',' => items.push(std::mem::take(&mut cur)),
            _ => cur.push(ch),
        }
    }
    items.push(cur);
    items
        .into_iter()
        .map(|i| i.trim().to_string())
        .filter(|i| !i.is_empty())
        .collect()
}

/// The items an Edit Custom Lists entry box holds: one per line, or
/// separated by commas.
pub fn parse_list_entries(text: &str) -> Vec<String> {
    text.split(['\n', ','])
        .map(|i| i.trim().to_string())
        .filter(|i| !i.is_empty())
        .collect()
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

    #[test]
    fn custom_lists_round_trip_and_escape_commas() {
        let lists = vec![
            vec!["North".to_string(), "East".into(), "South".into()],
            vec!["a, b".to_string(), "c\\d".into()],
            // Imported from cells with line breaks (#707 r6 m1).
            vec![
                "two\nlines".to_string(),
                "cr\r\nlf".into(),
                "back\\n".into(),
            ],
        ];
        let text = custom_lists_to_lines(&lists);
        assert_eq!(text.lines().count(), 3, "{text}");
        assert_eq!(custom_lists_from_text(&text), lists);
        // Alongside the other options, each reader takes its own keys.
        let both = format!("{}{text}", EditOptions::default().to_lines());
        assert_eq!(custom_lists_from_text(&both), lists);
        assert_eq!(EditOptions::from_text(&both), EditOptions::default());
        assert_eq!(
            parse_list_entries("Low\n Mid ,High,,"),
            vec!["Low", "Mid", "High"]
        );
    }
}
