//! Rewriting a field's raw XML in place (#1241): its cached result (Update
//! Field, F9) and its instruction (Edit Field). Everything outside the part
//! rewritten stays byte-identical, so a complex field stays complex and keeps
//! its switches, locks and the formatting of its code.
//!
//! Both refuse (`None`) rather than guess: a result that holds a bookmark, a
//! tracked change, a picture or a nested field is left as it is, since losing
//! any of those is worse than a stale value.

use crate::field::{FieldEvent, field_events};
use crate::xml::{Event, XmlParser};

/// One element of a slice of XML siblings: its byte span and its name.
struct El<'a> {
    start: usize,
    end: usize,
    name: &'a str,
}

/// The top-level elements of `xml`, a run of siblings. `None` when it is
/// malformed or holds character data other than whitespace between them.
fn siblings(xml: &str) -> Option<Vec<El<'_>>> {
    let mut p = XmlParser::new(xml);
    let mut out = Vec::new();
    loop {
        match p.next() {
            Event::Start => {
                let (start, name) = (p.start_pos(), p.name());
                if !p.skip_element_complete() {
                    return None;
                }
                out.push(El {
                    start,
                    end: p.pos(),
                    name,
                });
            }
            Event::Text if p.text().trim().is_empty() => {}
            Event::Text | Event::End => return None,
            Event::Eof => break,
        }
    }
    (!p.is_malformed()).then_some(out)
}

/// The children of the one element `xml` is (a `w:r`), with spans relative
/// to `xml`, and the end of its start tag. `None` when malformed or empty.
fn children(xml: &str) -> Option<(usize, Vec<El<'_>>)> {
    let mut p = XmlParser::new(xml);
    if p.next() != Event::Start {
        return None;
    }
    let open_end = p.pos();
    let mut out = Vec::new();
    loop {
        match p.next() {
            Event::Start => {
                let (start, name) = (p.start_pos(), p.name());
                if !p.skip_element_complete() {
                    return None;
                }
                out.push(El {
                    start,
                    end: p.pos(),
                    name,
                });
            }
            Event::Text => {}
            Event::End => break,
            Event::Eof => return None,
        }
    }
    (!p.is_malformed()).then_some((open_end, out))
}

/// The children a result run may hold for its text to be replaced: its
/// formatting, its text, and marks that show nothing of their own.
fn plain_result_run(run: &str) -> bool {
    children(run).is_some_and(|(_, kids)| {
        kids.iter().all(|k| {
            matches!(
                k.name,
                "w:rPr" | "w:t" | "w:lastRenderedPageBreak" | "w:softHyphen" | "w:noBreakHyphen"
            )
        })
    })
}

/// The `<w:rPr>…</w:rPr>` of run `run`, verbatim, or "" with none.
fn rpr_of(run: &str) -> &str {
    children(run)
        .and_then(|(_, kids)| {
            kids.iter()
                .find(|k| k.name == "w:rPr")
                .map(|k| &run[k.start..k.end])
        })
        .unwrap_or("")
}

/// One run holding `text`, formatted with the verbatim `rpr`.
fn result_run(rpr: &str, text: &str) -> String {
    let mut s = String::from("<w:r>");
    s.push_str(rpr);
    let edge_space = text.starts_with(char::is_whitespace) || text.ends_with(char::is_whitespace);
    s.push_str(if edge_space {
        "<w:t xml:space=\"preserve\">"
    } else {
        "<w:t>"
    });
    crate::serialize::esc_text(text, &mut s);
    s.push_str("</w:t></w:r>");
    s
}

/// The new result runs for `region`, the elements a field's result is made
/// of: one run with `text` and the first run's formatting. `None` when the
/// region holds anything but plain runs and proofing marks.
fn new_result(raw: &str, region: &[El<'_>], text: &str) -> Option<String> {
    let mut rpr = None;
    for el in region {
        match el.name {
            "w:proofErr" => {}
            "w:r" if plain_result_run(&raw[el.start..el.end]) => {
                rpr.get_or_insert_with(|| rpr_of(&raw[el.start..el.end]));
            }
            _ => return None,
        }
    }
    Some(result_run(rpr.unwrap_or(""), text))
}

/// Whether `raw` is a `w:fldSimple` element (else a complex field's runs, or a
/// `w:sym` run the loader keeps as a field).
fn is_simple(raw: &str) -> bool {
    raw.trim_start().starts_with("<w:fldSimple")
}

/// For a complex field's siblings: the index of the run holding the
/// outermost field's `begin`, `separate` (if any) and `end`. `None` when a
/// run holds more than one of those, or the field does not close.
fn complex_marks(raw: &str, els: &[El<'_>]) -> Option<(usize, Option<usize>, usize)> {
    let (mut begin, mut separate) = (None, None);
    let mut depth = 0usize;
    for (i, el) in els.iter().enumerate() {
        if el.name != "w:r" {
            continue;
        }
        let mut marks = 0;
        for event in field_events(&raw[el.start..el.end]) {
            match event {
                FieldEvent::Begin => {
                    depth += 1;
                    if depth == 1 {
                        begin = Some(i);
                        marks += 1;
                    }
                }
                FieldEvent::Separate if depth == 1 => {
                    separate = Some(i);
                    marks += 1;
                }
                FieldEvent::End => {
                    depth = depth.checked_sub(1)?;
                    if depth == 0 {
                        marks += 1;
                        if marks > 1 {
                            return None;
                        }
                        return Some((begin?, separate, i));
                    }
                }
                _ => {}
            }
        }
        if marks > 1 {
            return None;
        }
    }
    None
}

/// `raw` (a field's XML, as [`crate::model::Inline::Field`] keeps it) with
/// its cached result replaced by `text`, in one run formatted as the first
/// result run was. For a `w:fldSimple` the result is its content; for a
/// complex field, what lies between the outermost field's `separate` and its
/// `end`. The instruction and everything around the result are kept byte for
/// byte, and `w:proofErr` marks inside the result are dropped.
///
/// `None` (keep the field as it is) for a `w:sym` run, a complex field with
/// no `separate`, or a result holding anything but plain text runs: a
/// bookmark, a tracked change, a comment range, a tab, a picture, a nested
/// field.
pub fn with_result(raw: &str, text: &str) -> Option<String> {
    if is_simple(raw) {
        let els = siblings(raw)?;
        let [el] = els.as_slice() else {
            return None;
        };
        let (open_end, kids) = children(&raw[el.start..el.end])?;
        let open_end = el.start + open_end;
        if raw[..open_end].ends_with("/>") {
            return None; // `<w:fldSimple …/>`: no result to replace.
        }
        let kids: Vec<El<'_>> = kids
            .into_iter()
            .map(|k| El {
                start: el.start + k.start,
                end: el.start + k.end,
                name: k.name,
            })
            .collect();
        let close = kids.last().map_or(open_end, |k| k.end);
        let runs = new_result(raw, &kids, text)?;
        return Some(format!("{}{runs}{}", &raw[..open_end], &raw[close..]));
    }
    if !raw.contains("<w:fldChar") {
        return None; // a `w:sym` run
    }
    let els = siblings(raw)?;
    let (_, separate, end) = complex_marks(raw, &els)?;
    let separate = separate?;
    let region = &els[separate + 1..end];
    let runs = new_result(raw, region, text)?;
    Some(format!(
        "{}{runs}{}",
        &raw[..els[separate].end],
        &raw[els[end].start..]
    ))
}

/// `raw` with its instruction replaced by `instr`: a `w:fldSimple`'s
/// `w:instr`, or a complex field's `w:instrText`, which collapses into the
/// first one (`xml:space="preserve"`); the runs that held only the others
/// go. `None` for a `w:sym` run, or a nested field in the instruction.
pub fn with_instr(raw: &str, instr: &str) -> Option<String> {
    if is_simple(raw) {
        let at = raw.find("<w:fldSimple")?;
        let tag_end = at + raw[at..].find('>')?;
        let tag = &raw[at..tag_end];
        let (k, q) = ["w:instr=\"", "w:instr='"]
            .iter()
            .filter_map(|k| tag.find(k).map(|i| (i, *k)))
            .min_by_key(|(i, _)| *i)
            .map(|(i, k)| (at + i + k.len(), k.as_bytes()[k.len() - 1] as char))?;
        let v_end = k + raw[k..].find(q)?;
        let mut esc = String::new();
        crate::serialize::esc_attr(instr, &mut esc);
        if q == '\'' {
            esc = esc.replace('\'', "&apos;");
        }
        return Some(format!("{}{esc}{}", &raw[..k], &raw[v_end..]));
    }
    if !raw.contains("<w:fldChar") {
        return None;
    }
    let els = siblings(raw)?;
    let (begin, separate, end) = complex_marks(raw, &els)?;
    let code_end = separate.unwrap_or(end);
    let mut out = String::from(&raw[..els[begin].start]);
    let mut first = true;
    let mut cursor = els[begin].start;
    for el in &els[begin..=code_end] {
        if el.name != "w:r" {
            continue;
        }
        let run = &raw[el.start..el.end];
        let events = field_events(run);
        let nested = events
            .iter()
            .filter(|e| matches!(e, FieldEvent::Begin))
            .count()
            > usize::from(el.start == els[begin].start);
        if nested {
            return None;
        }
        if !events.iter().any(|e| matches!(e, FieldEvent::Instr(_))) {
            continue;
        }
        let (_, kids) = children(run)?;
        let only_code = kids
            .iter()
            .all(|k| matches!(k.name, "w:rPr" | "w:instrText"));
        out.push_str(&raw[cursor..el.start]);
        cursor = el.end;
        if !first && only_code {
            continue; // a later piece of the code: its run goes.
        }
        // Rewrite this run: the first instrText holds the code when this is
        // the first such run, and every other instrText goes.
        let mut run_out = String::new();
        let mut at = 0;
        for k in kids.iter().filter(|k| k.name == "w:instrText") {
            run_out.push_str(&run[at..k.start]);
            if first {
                run_out.push_str("<w:instrText xml:space=\"preserve\">");
                crate::serialize::esc_text(instr, &mut run_out);
                run_out.push_str("</w:instrText>");
                first = false;
            }
            at = k.end;
        }
        run_out.push_str(&run[at..]);
        out.push_str(&run_out);
    }
    if first {
        return None; // no instrText to rewrite
    }
    out.push_str(&raw[cursor..]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::field::instr_of;

    /// A complex field: `begin`, its code in two pieces, `separate`, the
    /// result `res` and `end`.
    fn complex(res: &str) -> String {
        format!(
            "<w:r><w:fldChar w:fldCharType=\"begin\"/></w:r>\
             <w:r><w:rPr><w:b/></w:rPr><w:instrText xml:space=\"preserve\"> DATE </w:instrText></w:r>\
             <w:r><w:instrText>\\@ \"M/d/yyyy\" \\* MERGEFORMAT </w:instrText></w:r>\
             <w:r><w:fldChar w:fldCharType=\"separate\"/></w:r>\
             {res}\
             <w:r><w:fldChar w:fldCharType=\"end\"/></w:r>"
        )
    }

    const HEAD_END: &str = "<w:r><w:fldChar w:fldCharType=\"separate\"/></w:r>";
    const TAIL: &str = "<w:r><w:fldChar w:fldCharType=\"end\"/></w:r>";

    #[test]
    fn a_simple_fields_result_becomes_one_run_with_its_first_runs_format_1241() {
        let raw = "<w:fldSimple w:instr=\" = 2+2 \"><w:r w:rsidR=\"00A1\"><w:rPr><w:i/><w:color w:val=\"FF0000\"/></w:rPr><w:t>3</w:t></w:r><w:proofErr w:type=\"spellStart\"/><w:r><w:t>9</w:t></w:r></w:fldSimple>";
        assert_eq!(
            with_result(raw, "4").as_deref(),
            Some(
                "<w:fldSimple w:instr=\" = 2+2 \"><w:r><w:rPr><w:i/><w:color w:val=\"FF0000\"/></w:rPr><w:t>4</w:t></w:r></w:fldSimple>"
            )
        );
    }

    #[test]
    fn a_complex_fields_result_is_replaced_and_its_code_kept_byte_for_byte_1241() {
        let res = "<w:r><w:rPr><w:noProof/><w:sz w:val=\"28\"/></w:rPr><w:t>1/1/2000</w:t></w:r><w:r><w:t>x</w:t></w:r>";
        let raw = complex(res);
        let out = with_result(&raw, "10/14/2026").unwrap();
        let head = &raw[..raw.find(HEAD_END).unwrap() + HEAD_END.len()];
        assert_eq!(
            out,
            format!(
                "{head}<w:r><w:rPr><w:noProof/><w:sz w:val=\"28\"/></w:rPr><w:t>10/14/2026</w:t></w:r>{TAIL}"
            )
        );
        assert_eq!(
            instr_of(&out).as_deref(),
            Some(" DATE \\@ \"M/d/yyyy\" \\* MERGEFORMAT ")
        );
    }

    #[test]
    fn a_result_is_escaped_and_keeps_its_edge_spaces_1241() {
        let raw = complex("<w:r><w:t>old</w:t></w:r>");
        let out = with_result(&raw, " a<b&c ").unwrap();
        assert!(out.contains("<w:r><w:t xml:space=\"preserve\"> a&lt;b&amp;c </w:t></w:r>"));
    }

    #[test]
    fn a_result_holding_more_than_text_runs_is_left_alone_1241() {
        for res in [
            "<w:bookmarkStart w:id=\"1\" w:name=\"b\"/><w:r><w:t>1</w:t></w:r><w:bookmarkEnd w:id=\"1\"/>",
            "<w:del w:id=\"1\" w:author=\"a\"><w:r><w:delText>1</w:delText></w:r></w:del><w:r><w:t>2</w:t></w:r>",
            "<w:ins w:id=\"1\" w:author=\"a\"><w:r><w:t>1</w:t></w:r></w:ins>",
            "<w:commentRangeStart w:id=\"0\"/><w:r><w:t>1</w:t></w:r>",
            "<w:r><w:t>1</w:t><w:tab/><w:t>2</w:t></w:r>",
            "<w:r><w:drawing/></w:r>",
            // A nested field in the result.
            "<w:r><w:fldChar w:fldCharType=\"begin\"/></w:r><w:r><w:instrText>PAGE</w:instrText></w:r>\
             <w:r><w:fldChar w:fldCharType=\"separate\"/></w:r><w:r><w:t>1</w:t></w:r>\
             <w:r><w:fldChar w:fldCharType=\"end\"/></w:r>",
        ] {
            assert_eq!(with_result(&complex(res), "x"), None, "{res}");
        }
        let nested_simple = "<w:fldSimple w:instr=\"A\"><w:fldSimple w:instr=\"B\"><w:r><w:t>1</w:t></w:r></w:fldSimple></w:fldSimple>";
        assert_eq!(with_result(nested_simple, "x"), None);
        let marked_simple = "<w:fldSimple w:instr=\"A\"><w:bookmarkStart w:id=\"1\" w:name=\"b\"/><w:r><w:t>1</w:t></w:r></w:fldSimple>";
        assert_eq!(with_result(marked_simple, "x"), None);
    }

    #[test]
    fn a_field_with_no_separate_or_a_symbol_is_left_alone_1241() {
        let unseparated = "<w:r><w:fldChar w:fldCharType=\"begin\"/></w:r>\
             <w:r><w:instrText>XE \"a\"</w:instrText></w:r>\
             <w:r><w:fldChar w:fldCharType=\"end\"/></w:r>";
        assert_eq!(with_result(unseparated, "x"), None);
        let sym = "<w:r><w:sym w:font=\"Symbol\" w:char=\"F0B7\"/></w:r>";
        assert_eq!(with_result(sym, "x"), None);
        assert_eq!(with_instr(sym, "x"), None);
    }

    #[test]
    fn a_simple_fields_instruction_is_its_escaped_attribute_1241() {
        let raw = "<w:fldSimple w:instr=\" PAGE \" w:dirty=\"true\"><w:r><w:t>1</w:t></w:r></w:fldSimple>";
        let out = with_instr(raw, "DATE \\@ \"d MMM\" <x>").unwrap();
        assert_eq!(
            out,
            "<w:fldSimple w:instr=\"DATE \\@ &quot;d MMM&quot; &lt;x&gt;\" w:dirty=\"true\"><w:r><w:t>1</w:t></w:r></w:fldSimple>"
        );
        assert_eq!(instr_of(&out).as_deref(), Some("DATE \\@ \"d MMM\" <x>"));
    }

    #[test]
    fn a_complex_fields_code_collapses_into_its_first_piece_1241() {
        let raw = complex("<w:r><w:t>1/1/2000</w:t></w:r>");
        let out = with_instr(&raw, " TIME \\@ \"h:mm\" ").unwrap();
        assert_eq!(
            out,
            "<w:r><w:fldChar w:fldCharType=\"begin\"/></w:r>\
             <w:r><w:rPr><w:b/></w:rPr><w:instrText xml:space=\"preserve\"> TIME \\@ \"h:mm\" </w:instrText></w:r>\
             <w:r><w:fldChar w:fldCharType=\"separate\"/></w:r>\
             <w:r><w:t>1/1/2000</w:t></w:r>\
             <w:r><w:fldChar w:fldCharType=\"end\"/></w:r>"
        );
        assert_eq!(instr_of(&out).as_deref(), Some(" TIME \\@ \"h:mm\" "));
    }

    #[test]
    fn a_code_sharing_its_run_with_begin_keeps_the_begin_1241() {
        let raw = "<w:r><w:fldChar w:fldCharType=\"begin\"/><w:instrText>PAGE</w:instrText></w:r>\
             <w:r><w:fldChar w:fldCharType=\"separate\"/></w:r><w:r><w:t>1</w:t></w:r>\
             <w:r><w:fldChar w:fldCharType=\"end\"/></w:r>";
        let out = with_instr(raw, "NUMPAGES").unwrap();
        assert!(out.starts_with(
            "<w:r><w:fldChar w:fldCharType=\"begin\"/><w:instrText xml:space=\"preserve\">NUMPAGES</w:instrText></w:r>"
        ));
        assert_eq!(instr_of(&out).as_deref(), Some("NUMPAGES"));
    }

    #[test]
    fn a_code_holding_a_nested_field_is_left_alone_1241() {
        let raw = "<w:r><w:fldChar w:fldCharType=\"begin\"/></w:r>\
             <w:r><w:instrText>= </w:instrText></w:r>\
             <w:r><w:fldChar w:fldCharType=\"begin\"/></w:r><w:r><w:instrText>PAGE</w:instrText></w:r>\
             <w:r><w:fldChar w:fldCharType=\"separate\"/></w:r><w:r><w:t>1</w:t></w:r>\
             <w:r><w:fldChar w:fldCharType=\"end\"/></w:r>\
             <w:r><w:instrText> + 1</w:instrText></w:r>\
             <w:r><w:fldChar w:fldCharType=\"separate\"/></w:r><w:r><w:t>2</w:t></w:r>\
             <w:r><w:fldChar w:fldCharType=\"end\"/></w:r>";
        assert_eq!(with_instr(raw, "= 3"), None);
        // Its result is plain, so the result can still be refreshed.
        assert!(with_result(raw, "3").unwrap().ends_with(
            "<w:r><w:fldChar w:fldCharType=\"separate\"/></w:r><w:r><w:t>3</w:t></w:r><w:r><w:fldChar w:fldCharType=\"end\"/></w:r>"
        ));
    }
}
