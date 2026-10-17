//! `field-read`: what a terminal editor's text prompts hold, for the UI
//! harness's terminal cases (#1204).
//!
//! The harness types into a terminal editor through a pseudo-terminal, as a
//! person does, and reads back what the prompt then holds with this verb. It
//! is read-only and answers the same shape the suite's `field-read
//! {"input":id}` does (`input`, `value`, `focused`, `caret`, `selection`,
//! in chars, `null` when the field has none or is not open), plus what only a
//! terminal needs:
//!
//! - `keys`, `clicks`, `frames`: key presses handled, mouse button-downs
//!   handled and frames drawn, since start. The harness writes a key's bytes
//!   and waits until `keys` has moved, then until a frame newer than that has
//!   been drawn, so what it reads next is what the screen shows.
//! - `at`: `{col,row}` (0-based) of the field's first character as last
//!   drawn, where a click on it lands; `null` when it is not on screen.
//! - `dirty`: the document has unsaved changes.
//! - `kind`: which prompt of a shared prompt field is open, where there are
//!   several (xlsxy's minibuffer, yppxy's prompt bar).
//!
//! With no `input` it answers only the counters and the editor's input ids.
//! The editors keep the counters in an [`InputCounts`] and describe each
//! field as a [`Field`]; [`reply`] does the rest, so the three agree.

use crate::json::Json;

/// What the event loop has handled so far.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InputCounts {
    pub keys: u64,
    pub clicks: u64,
    pub frames: u64,
}

/// One field, as it stands.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Field {
    /// Its text; `None` when the field is not open.
    pub value: Option<String>,
    pub focused: bool,
    /// The caret, in chars; `None` when the field is not open.
    pub caret: Option<usize>,
    /// The selected chars `[start, end)`; `None` when nothing is selected.
    pub selection: Option<(usize, usize)>,
    /// Its first character's cell as last drawn.
    pub at: Option<(u16, u16)>,
    /// The open prompt's kind, for a field several prompts share.
    pub kind: Option<String>,
}

impl Field {
    /// A field that is not open.
    pub fn closed() -> Field {
        Field::default()
    }

    /// An open field that only appends and pops: the caret is always at the
    /// end, and nothing can be selected.
    pub fn append_only(value: &str, focused: bool) -> Field {
        Field {
            value: Some(value.to_string()),
            focused,
            caret: Some(value.chars().count()),
            ..Field::default()
        }
    }

    /// An open field that edits at a caret (`caret` chars in); nothing can be
    /// selected.
    pub fn with_caret(value: &str, focused: bool, caret: usize) -> Field {
        Field {
            value: Some(value.to_string()),
            focused,
            caret: Some(caret),
            ..Field::default()
        }
    }

    /// Drawn with its first character at `at`.
    pub fn at(mut self, at: Option<(u16, u16)>) -> Field {
        self.at = at;
        self
    }

    pub fn kind(mut self, kind: impl Into<String>) -> Field {
        self.kind = Some(kind.into());
        self
    }
}

/// `field-read`'s reply. `ids` are the editor's input ids; `read` describes
/// one of them.
pub fn reply(
    args: &Json,
    ids: &[&str],
    counts: InputCounts,
    dirty: bool,
    read: impl FnOnce(&str) -> Field,
) -> Result<Json, String> {
    let common = |mut pairs: Vec<(&str, Json)>| {
        pairs.push(("keys", Json::Num(counts.keys as f64)));
        pairs.push(("clicks", Json::Num(counts.clicks as f64)));
        pairs.push(("frames", Json::Num(counts.frames as f64)));
        pairs.push(("dirty", Json::Bool(dirty)));
        Json::obj(pairs)
    };
    let Some(input) = args.get("input") else {
        return Ok(common(vec![(
            "inputs",
            Json::Arr(ids.iter().map(|i| Json::Str(i.to_string())).collect()),
        )]));
    };
    let id = input.as_str().ok_or("'input' must be an input id")?;
    if !ids.contains(&id) {
        return Err(format!("unknown input '{id}'; inputs: {}", ids.join(", ")));
    }
    let f = read(id);
    let num = |n: Option<usize>| n.map_or(Json::Null, |n| Json::Num(n as f64));
    Ok(common(vec![
        ("input", Json::Str(id.to_string())),
        ("value", f.value.map_or(Json::Null, Json::Str)),
        ("focused", Json::Bool(f.focused)),
        ("caret", num(f.caret)),
        (
            "selection",
            f.selection.map_or(Json::Null, |(a, b)| {
                Json::Arr(vec![Json::Num(a as f64), Json::Num(b as f64)])
            }),
        ),
        (
            "at",
            f.at.map_or(Json::Null, |(col, row)| {
                Json::obj(vec![
                    ("col", Json::Num(f64::from(col))),
                    ("row", Json::Num(f64::from(row))),
                ])
            }),
        ),
        ("kind", f.kind.map_or(Json::Null, Json::Str)),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDS: &[&str] = &["app-find", "app-prompt"];

    fn counts() -> InputCounts {
        InputCounts {
            keys: 3,
            clicks: 1,
            frames: 9,
        }
    }

    #[test]
    fn without_an_input_it_answers_the_counters_and_the_ids() {
        let r = reply(&Json::obj(vec![]), IDS, counts(), true, |_| unreachable!()).unwrap();
        assert_eq!(
            r.to_string(),
            r#"{"inputs":["app-find","app-prompt"],"keys":3,"clicks":1,"frames":9,"dirty":true}"#
        );
    }

    #[test]
    fn an_open_field_reports_its_text_caret_and_cell() {
        let args = Json::obj(vec![("input", Json::Str("app-find".into()))]);
        let r = reply(&args, IDS, counts(), false, |id| {
            assert_eq!(id, "app-find");
            Field::append_only("Ab1 é", true).at(Some((7, 39)))
        })
        .unwrap();
        assert_eq!(
            r.to_string(),
            r#"{"input":"app-find","value":"Ab1 é","focused":true,"caret":5,"selection":null,"at":{"col":7,"row":39},"kind":null,"keys":3,"clicks":1,"frames":9,"dirty":false}"#
        );
    }

    #[test]
    fn a_closed_field_is_all_null_and_unfocused() {
        let args = Json::obj(vec![("input", Json::Str("app-prompt".into()))]);
        let r = reply(&args, IDS, counts(), false, |_| Field::closed()).unwrap();
        assert_eq!(r.get("value"), Some(&Json::Null));
        assert_eq!(r.get("caret"), Some(&Json::Null));
        assert_eq!(r.get("at"), Some(&Json::Null));
        assert_eq!(r.get("focused").and_then(Json::as_bool), Some(false));
    }

    #[test]
    fn an_unknown_input_is_refused_with_the_list() {
        let args = Json::obj(vec![("input", Json::Str("nope".into()))]);
        let e = reply(&args, IDS, counts(), false, |_| Field::closed()).unwrap_err();
        assert_eq!(e, "unknown input 'nope'; inputs: app-find, app-prompt");
    }
}
