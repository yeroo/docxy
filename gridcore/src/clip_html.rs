//! A copied range as an HTML table, and another app's HTML or RTF table read
//! back as cell text (#1248).
//!
//! [`html_table`] is the fragment a sheet copy puts beside its TSV on the
//! system clipboard: display values only, never formulas, each cell with the
//! inline style Excel, Numbers and Pages read (bold, italic, font, colour,
//! fill, alignment). [`parse_html_table`] and [`parse_rtf_table`] go the other
//! way for a paste: the first table's cell texts, by position, with no
//! styles. Both parsers are small tolerant scanners (unclosed tags, CF_HTML
//! headers, Excel's `class=xl65` attributes), not full HTML or RTF readers.

use crate::sheet::{Align, Cell, CellValue, Sheet, Styles, format_with};

/// The most cells a parsed table may span (colspan/rowspan fill included):
/// a hostile `rowspan` cannot ask for more memory than this.
const MAX_CELLS: usize = 4_000_000;

/// The attribute [`html_table`] puts on its table: docxy's own copy, whose
/// exact TSV beside it (with the `'` a text cell needs) a paste prefers.
const OWN_MARK: &str = "data-source=docxy";

/// Whether `html` holds a table [`html_table`] wrote.
pub fn is_own_table(html: &str) -> bool {
    html.contains(OWN_MARK)
}

/// `s` for an HTML text node or attribute: `&`, `<`, `>`, `"` escaped, a
/// line feed as `<br>`.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\r' => {}
            '\n' => out.push_str("<br>"),
            _ => out.push(c),
        }
    }
    out
}

/// The cell's text as displayed, line breaks as LF.
fn shown(cell: &Cell, styles: &Styles, date1904: bool) -> String {
    let text = format_with(&styles.xf(cell.style), &cell.value, date1904);
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// The inline `style` of a cell with `xf` holding `value`.
fn cell_style(xf: &crate::sheet::Xf, value: &CellValue) -> String {
    let mut css = String::new();
    if xf.bold {
        css.push_str("font-weight:700;");
    }
    if xf.italic {
        css.push_str("font-style:italic;");
    }
    if let Some((r, g, b)) = xf.color {
        css.push_str(&format!("color:#{r:02x}{g:02x}{b:02x};"));
    }
    if let Some((r, g, b)) = xf.fill {
        css.push_str(&format!("background:#{r:02x}{g:02x}{b:02x};"));
    }
    if let Some(size) = xf.font_size {
        css.push_str(&format!("font-size:{size}pt;"));
    }
    if let Some(name) = &xf.font_name {
        let name: String = name
            .chars()
            .filter(|c| !matches!(c, '\'' | '"' | '<' | '>' | '&' | ';' | '\\'))
            .collect();
        if !name.is_empty() {
            css.push_str(&format!("font-family:{name};"));
        }
    }
    match xf.align {
        Align::Left => css.push_str("text-align:left;"),
        Align::Center => css.push_str("text-align:center;"),
        Align::Right => css.push_str("text-align:right;"),
        Align::General if matches!(value, CellValue::Number(_)) => {
            css.push_str("text-align:right;")
        }
        Align::General => {}
    }
    // Text stays text where another app would read it as a number or a date.
    if matches!(value, CellValue::Text(_)) {
        css.push_str("mso-number-format:\"\\@\";");
    }
    css
}

/// The HTML table (a fragment, no `<html>`) for `rows` x `cols` of `sheet`:
/// each cell's display text, escaped, with its formatting as an inline style.
/// A formula shows only its result.
pub fn html_table(
    sheet: &Sheet,
    styles: &Styles,
    date1904: bool,
    rows: &[u32],
    cols: &[u32],
) -> String {
    let mut out = format!("<table {OWN_MARK} border=0 cellpadding=0 cellspacing=0>");
    for &r in rows {
        out.push_str("<tr>");
        for &c in cols {
            match sheet.cell(r, c) {
                Some(cell) if !cell.is_blank() => {
                    let xf = styles.xf(cell.style);
                    let css = cell_style(&xf, &cell.value);
                    let attr = if css.is_empty() {
                        String::new()
                    } else {
                        format!(" style='{css}'")
                    };
                    out.push_str(&format!(
                        "<td{attr}>{}</td>",
                        escape(&shown(cell, styles, date1904))
                    ));
                }
                Some(cell) => {
                    let xf = styles.xf(cell.style);
                    let css = cell_style(&xf, &cell.value);
                    if css.is_empty() {
                        out.push_str("<td></td>");
                    } else {
                        out.push_str(&format!("<td style='{css}'></td>"));
                    }
                }
                None => out.push_str("<td></td>"),
            }
        }
        out.push_str("</tr>");
    }
    out.push_str("</table>");
    out
}

/// A table being read: rows of cells by position, with the cells a
/// `colspan`/`rowspan` covers marked taken.
#[derive(Default)]
struct Grid {
    rows: Vec<Vec<Option<String>>>,
    /// Cells allocated so far, and cells visited filling spans: both are
    /// bounded by [`MAX_CELLS`], so a hostile table costs neither memory nor
    /// time without limit.
    cells: usize,
    work: usize,
}

impl Grid {
    fn ensure(&mut self, r: usize, c: usize) -> bool {
        let have = self.rows.get(r).map_or(0, Vec::len);
        let grow = (r + 1).saturating_sub(self.rows.len()) + (c + 1).saturating_sub(have);
        self.cells += grow;
        if self.cells > MAX_CELLS {
            return false;
        }
        while self.rows.len() <= r {
            self.rows.push(Vec::new());
        }
        while self.rows[r].len() <= c {
            self.rows[r].push(None);
        }
        true
    }

    fn taken(&self, r: usize, c: usize) -> bool {
        self.rows
            .get(r)
            .and_then(|row| row.get(c))
            .is_some_and(Option::is_some)
    }

    /// Put `text` at the first free column of row `r` from `c`, fill the
    /// span with empty cells, and return the column after it.
    fn place(
        &mut self,
        r: usize,
        mut c: usize,
        text: String,
        cs: usize,
        rs: usize,
    ) -> Option<usize> {
        self.work += cs * rs;
        if self.work > MAX_CELLS {
            return None;
        }
        while self.taken(r, c) {
            c += 1;
        }
        for dr in 0..rs {
            for dc in 0..cs {
                if !self.ensure(r + dr, c + dc) {
                    return None;
                }
                let slot = &mut self.rows[r + dr][c + dc];
                if slot.is_none() {
                    *slot = Some(String::new());
                }
            }
        }
        self.rows[r][c] = Some(text);
        Some(c + cs)
    }

    /// The rows padded to the widest, or `None` for no cell or for more
    /// cells than [`MAX_CELLS`] once padded (a thousand rows of one cell and
    /// one of a thousand would otherwise make a million).
    fn finish(self) -> Option<Vec<Vec<String>>> {
        let width = self.rows.iter().map(Vec::len).max().unwrap_or(0);
        if width == 0 || self.rows.len().saturating_mul(width) > MAX_CELLS {
            return None;
        }
        Some(
            self.rows
                .into_iter()
                .map(|row| {
                    let mut row: Vec<String> =
                        row.into_iter().map(Option::unwrap_or_default).collect();
                    row.resize(width, String::new());
                    row
                })
                .collect(),
        )
    }
}

/// The decoded text of an HTML entity body (between `&` and `;`).
fn entity(body: &str) -> Option<char> {
    if let Some(num) = body.strip_prefix('#') {
        let code = match num.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => num.parse().ok()?,
        };
        return char::from_u32(code).filter(|&c| c != '\0');
    }
    Some(match body {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{a0}',
        _ => return None,
    })
}

/// `text` with entities decoded. An `&` looks only a few bytes ahead for its
/// `;`, so a run of bare `&`s costs no more than its length.
fn decode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let decoded = rest.as_bytes()[1..]
            .iter()
            .take(11)
            .position(|&b| b == b';')
            .and_then(|n| entity(&rest[1..1 + n]).map(|c| (c, n + 2)));
        match decoded {
            Some((c, used)) => {
                out.push(c);
                rest = &rest[used..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// A tag's name, lower-cased, and whether it closes (`</td>`).
fn tag_name(tag: &str) -> (String, bool) {
    let (closing, body) = match tag.strip_prefix('/') {
        Some(body) => (true, body),
        None => (false, tag),
    };
    let name: String = body
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    (name, closing)
}

/// The number in attribute `name` (`colspan=2`, `rowspan="3"`) of `tag`, at
/// least 1, at most 1000.
fn span(tag: &str, name: &str) -> usize {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find(name) {
        let at = from + i;
        let before_ok = at == 0 || !lower.as_bytes()[at - 1].is_ascii_alphanumeric();
        let rest = lower[at + name.len()..].trim_start();
        if before_ok {
            if let Some(value) = rest.strip_prefix('=') {
                let value = value.trim_start().trim_start_matches(['"', '\'']);
                let digits: String = value.chars().take_while(char::is_ascii_digit).collect();
                return digits.parse::<usize>().unwrap_or(1).clamp(1, 1000);
            }
        }
        from = at + name.len();
    }
    1
}

/// A `<br>` or block end inside a cell's text, until [`collapse`] makes it a
/// line feed.
const BREAK: char = '\u{1}';

/// A cell's text with HTML whitespace collapsed: a run of blanks is one
/// space, the cell's ends are trimmed, a [`BREAK`] is a line feed, and a
/// no-break space (`&nbsp;`) is a space that survives all of that. A cell of
/// nothing else (Excel writes `&nbsp;` for an empty one) is empty.
fn collapse(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut blank = true;
    for c in raw.chars() {
        if c == BREAK {
            while out.ends_with(' ') {
                out.pop();
            }
            out.push(BREAK);
            blank = true;
        } else if c.is_whitespace() && c != '\u{a0}' {
            if !blank {
                out.push(' ');
                blank = true;
            }
        } else {
            out.push(c);
            blank = false;
        }
    }
    let text = out.trim_matches([' ', BREAK]).replace(BREAK, "\n");
    if text.chars().all(char::is_whitespace) {
        return String::new();
    }
    text.replace('\u{a0}', " ")
}

/// The cell texts of the table that is all of `html`, rows by columns (short
/// rows padded; a `colspan`/`rowspan` fills the cells it covers with empty
/// text), or `None` for no table, no cell, more than one table, or any text
/// outside the table (a heading or a paragraph beside it: the copy is more
/// than a table, so its text is pasted instead). Entities are decoded, source
/// line breaks are blanks and a `<br>` is a line feed; a table inside a cell
/// is part of that cell's text. A CF_HTML header, `<html>`, `<title>` and
/// `<style>` are not text, and an unclosed `<td>`, `<tr>` or `<table>` ends
/// at the next one.
#[allow(unused_assignments)]
pub fn parse_html_table(html: &str) -> Option<Vec<Vec<String>>> {
    let lower = html.to_ascii_lowercase();
    let bytes = html.as_bytes();
    let mut grid = Grid::default();
    let mut row: Option<usize> = None;
    let mut col = 0usize;
    let mut cell: Option<(String, usize, usize)> = None;
    let mut next_row = 0usize;
    let (mut depth, mut tables, mut outside) = (0usize, 0usize, false);
    let mut i = 0;
    macro_rules! close_cell {
        () => {
            if let Some((text, cs, rs)) = cell.take() {
                let r = row?;
                col = grid.place(r, col, collapse(&decode(&text)), cs, rs)?;
            }
        };
    }
    macro_rules! text {
        ($t:expr) => {
            let t: &str = $t;
            if let Some((cell, ..)) = cell.as_mut() {
                cell.extend(t.chars().map(|c| {
                    if matches!(c, '\r' | '\n' | '\t') {
                        ' '
                    } else {
                        c
                    }
                }));
            } else if depth == 0 && !decode(t).chars().all(char::is_whitespace) {
                outside = true;
            }
        };
    }
    while i < bytes.len() {
        let is_tag = bytes[i] == b'<'
            && bytes
                .get(i + 1)
                .is_some_and(|&n| n.is_ascii_alphabetic() || matches!(n, b'/' | b'!' | b'?'));
        if !is_tag {
            let end = html[i + 1..].find('<').map_or(html.len(), |n| i + 1 + n);
            text!(&html[i..end]);
            i = end;
            continue;
        }
        if html[i..].starts_with("<!--") {
            i = html[i..].find("-->").map_or(html.len(), |n| i + n + 3);
            continue;
        }
        let Some(n) = html[i..].find('>') else {
            break;
        };
        let tag = &html[i + 1..i + n];
        let (name, closing) = tag_name(tag);
        i += n + 1;
        match (name.as_str(), closing) {
            ("table", false) => {
                if depth == 0 {
                    tables += 1;
                    if tables > 1 {
                        return None;
                    }
                }
                depth += 1;
            }
            ("table", true) => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    close_cell!();
                }
            }
            ("style" | "script" | "title", false) => {
                let close = format!("</{name}");
                i = lower[i..].find(&close).map_or(html.len(), |n| i + n);
            }
            // A nested table's cells belong to the outer cell's text: only
            // the outermost table's own rows and cells make the grid.
            _ if depth != 1 => {}
            ("tr", false) => {
                close_cell!();
                row = Some(next_row);
                next_row += 1;
                col = 0;
            }
            ("tr", true) => close_cell!(),
            ("td" | "th", false) => {
                close_cell!();
                if row.is_none() {
                    row = Some(next_row);
                    next_row += 1;
                    col = 0;
                }
                cell = Some((String::new(), span(tag, "colspan"), span(tag, "rowspan")));
            }
            ("td" | "th", true) => close_cell!(),
            ("br", _) | ("p" | "div", true) => {
                if let Some((text, ..)) = cell.as_mut() {
                    if !text.ends_with(BREAK) {
                        text.push(BREAK);
                    }
                }
            }
            _ => {}
        }
    }
    close_cell!();
    if outside {
        return None;
    }
    grid.finish()
}

/// What a `\'hh` byte reads as when its code page is not known.
const REPLACEMENT: char = '\u{fffd}';

/// The RTF groups whose text is not the document's.
fn skipped_destination(word: &str) -> bool {
    matches!(
        word,
        "fonttbl"
            | "colortbl"
            | "stylesheet"
            | "info"
            | "pict"
            | "header"
            | "footer"
            | "footnote"
            | "listtable"
            | "listoverridetable"
            | "rsidtbl"
            | "generator"
            | "themedata"
            | "colorschememapping"
            | "latentstyles"
            | "datastore"
            | "xmlnstbl"
            | "expandedcolortbl"
    )
}

/// The cell texts of the table that is all of `rtf` (rows of `\trowd`,
/// `\cell`s and `\row`), or `None` when the RTF holds no table or any text
/// outside it (a paragraph before or after: the copy is more than a table,
/// so its text is pasted instead). Control words and groups are dropped,
/// `\'hh` is read in the `\ansicpgN` code page (1252 without one), `\uN` as
/// UTF-16 (the fallback character after it skipped), `\par` and `\line` as a
/// line feed inside the cell. A `\trowd` repeated before `\row`, as Word
/// writes it, keeps the row's cells. A table inside a cell is part of that
/// cell's text. Rows are padded to the widest.
pub fn parse_rtf_table(rtf: &str) -> Option<Vec<Vec<String>>> {
    let b = rtf.as_bytes();
    let mut i = 0;
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut cur = String::new();
    let (mut seen_table, mut outside) = (false, false);
    let mut cells = 0usize;
    let mut skip_depth: Option<usize> = None;
    let mut depth = 0usize;
    let mut uc = 1usize;
    let mut uc_skip = 0usize;
    let mut page = 1252i32;
    // Pending high surrogate of a `\uN` pair.
    let mut high: Option<u16> = None;
    while i < b.len() {
        match b[i] {
            b'{' => {
                depth += 1;
                i += 1;
                if skip_depth.is_none() {
                    let rest = &rtf[i..];
                    let starred = rest.starts_with("\\*");
                    let word: String = rest
                        .strip_prefix('\\')
                        .unwrap_or("")
                        .chars()
                        .take_while(char::is_ascii_alphabetic)
                        .collect();
                    if starred || skipped_destination(&word) {
                        skip_depth = Some(depth);
                    }
                }
            }
            b'}' => {
                if skip_depth == Some(depth) {
                    skip_depth = None;
                }
                depth = depth.saturating_sub(1);
                i += 1;
            }
            b'\\' => {
                i += 1;
                let Some(ch) = rtf[i..].chars().next() else {
                    break;
                };
                if !ch.is_ascii_alphabetic() {
                    i += ch.len_utf8();
                    let skipping = skip_depth.is_some();
                    match ch {
                        '\'' => {
                            let hex: String = rtf[i..]
                                .chars()
                                .take(2)
                                .take_while(char::is_ascii_hexdigit)
                                .collect();
                            i += hex.len();
                            if uc_skip > 0 {
                                uc_skip -= 1;
                            } else if hex.len() == 2 && !skipping {
                                let byte = u8::from_str_radix(&hex, 16).unwrap_or(b'?');
                                cur.push(
                                    opccore::codepage::decode(page, byte).unwrap_or(REPLACEMENT),
                                );
                            }
                        }
                        '\\' | '{' | '}' if !skipping => {
                            if uc_skip > 0 {
                                uc_skip -= 1;
                            } else {
                                cur.push(ch);
                            }
                        }
                        '\n' | '\r' if !skipping => cur.push('\n'),
                        '~' if !skipping => cur.push(' '),
                        _ => {}
                    }
                    continue;
                }
                let word_start = i;
                while i < b.len() && b[i].is_ascii_alphabetic() {
                    i += 1;
                }
                let word = &rtf[word_start..i];
                let num_start = i;
                if b.get(i) == Some(&b'-') {
                    i += 1;
                }
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
                let param: Option<i32> = rtf[num_start..i].parse().ok();
                if b.get(i) == Some(&b' ') {
                    i += 1;
                }
                if skip_depth.is_some() {
                    continue;
                }
                match word {
                    "ansicpg" => page = param.unwrap_or(1252),
                    "trowd" => {
                        // Word repeats the row's definition before `\row`:
                        // only a row's first one ends the text before it.
                        if row.is_empty() {
                            if !cur.trim().is_empty() {
                                outside = true;
                            }
                            cur.clear();
                        }
                        seen_table = true;
                    }
                    "cell" if seen_table => {
                        row.push(cur.trim_matches(['\n', ' ']).to_string());
                        cur.clear();
                        cells += 1;
                    }
                    "row" if seen_table => {
                        if !cur.trim().is_empty() {
                            row.push(cur.trim_matches(['\n', ' ']).to_string());
                        }
                        cur.clear();
                        rows.push(std::mem::take(&mut row));
                        cells += 1;
                    }
                    "nestcell" | "nestrow" | "tab" => cur.push(' '),
                    "par" | "line" => cur.push('\n'),
                    "uc" => uc = param.unwrap_or(1).max(0) as usize,
                    "u" => {
                        if let Some(n) = param {
                            let unit = n as u16;
                            match (high.take(), unit) {
                                (Some(h), 0xDC00..=0xDFFF) => {
                                    if let Some(Ok(c)) = char::decode_utf16([h, unit]).next() {
                                        cur.push(c);
                                    }
                                }
                                (_, 0xD800..=0xDBFF) => high = Some(unit),
                                (_, u) => {
                                    if let Some(Ok(c)) = char::decode_utf16([u]).next() {
                                        cur.push(c);
                                    }
                                }
                            }
                            uc_skip = uc;
                        }
                    }
                    _ => {}
                }
                if cells > MAX_CELLS {
                    return None;
                }
            }
            b'\r' | b'\n' => i += 1,
            _ => {
                let ch = rtf[i..].chars().next().unwrap_or(' ');
                i += ch.len_utf8();
                if skip_depth.is_some() {
                    continue;
                }
                if uc_skip > 0 {
                    uc_skip -= 1;
                } else {
                    cur.push(ch);
                }
            }
        }
    }
    if !cur.trim().is_empty() {
        outside = true;
    }
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    if !seen_table || outside || width == 0 || rows.len().saturating_mul(width) > MAX_CELLS {
        return None;
    }
    for r in &mut rows {
        r.resize(width, String::new());
    }
    Some(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sheet::{Cell, Xf};

    fn sheet_with(cells: &[(u32, u32, Cell)]) -> Sheet {
        let mut sh = Sheet {
            name: "S".into(),
            ..Default::default()
        };
        for (r, c, cell) in cells {
            sh.set_cell(*r, *c, cell.clone());
        }
        sh
    }

    fn rows(r: &[&[&str]]) -> Vec<Vec<String>> {
        r.iter()
            .map(|row| row.iter().map(|c| c.to_string()).collect())
            .collect()
    }

    #[test]
    fn a_range_becomes_a_styled_table_of_display_text() {
        let mut styles = Styles::default();
        styles.intern(Xf::default());
        let bold = styles.intern(Xf {
            bold: true,
            color: Some((255, 0, 0)),
            fill: Some((0, 255, 0)),
            align: Align::Center,
            ..Default::default()
        });
        let mut b = Cell::text("a<b> & \"c\"\nd");
        b.style = bold;
        let sh = sheet_with(&[(0, 0, b), (0, 1, Cell::number(1.5))]);
        let html = html_table(&sh, &styles, false, &[0, 1], &[0, 1]);
        assert!(html.starts_with("<table"));
        assert!(is_own_table(&html));
        assert!(
            html.contains("font-weight:700;color:#ff0000;background:#00ff00;text-align:center;")
        );
        assert!(html.contains("a&lt;b&gt; &amp; &quot;c&quot;<br>d"));
        assert!(
            html.contains("<td style='text-align:right;'>1.5</td>"),
            "{html}"
        );
        // Text stays text in another app.
        assert!(html.contains("mso-number-format:\"\\@\";"));
        // Two rows, two cells each, the empty ones present.
        assert_eq!(html.matches("<tr>").count(), 2);
        assert_eq!(html.matches("<td").count(), 4);
        assert_eq!(parse_html_table(&html).unwrap()[0][0], "a<b> & \"c\"\nd");
    }

    #[test]
    fn only_our_own_table_is_marked() {
        assert!(!is_own_table("<table><tr><td>x</td></tr></table>"));
    }

    /// The TSV beside docxy's table carries a `'` for text that reads as a
    /// number (`entry::copy_field`); the table does not, since another app
    /// would show it. So a paste prefers the TSV to the table it marked.
    #[test]
    fn a_numeric_looking_text_has_no_apostrophe_in_the_table() {
        let styles = Styles::default();
        let sh = sheet_with(&[(0, 0, Cell::text("007"))]);
        let html = html_table(&sh, &styles, false, &[0], &[0]);
        assert!(html.contains(">007<") && !html.contains("'007"));
        assert!(is_own_table(&html));
    }

    #[test]
    fn a_formula_shows_only_its_result() {
        let styles = Styles::default();
        let mut cell = Cell::number(3.0);
        cell.formula = Some("1+2".into());
        let sh = sheet_with(&[(0, 0, cell)]);
        let html = html_table(&sh, &styles, false, &[0], &[0]);
        assert!(html.contains(">3<"));
        assert!(!html.contains("1+2"));
    }

    #[test]
    fn a_parsed_table_has_its_cells_by_position() {
        let html = "<html><head><title>t</title><style>td{x:y}</style></head><body>\
            <!--StartFragment--><table><tr><td class=xl65 style='x:y'>a &amp; b</td><td>1&nbsp;2</td></tr>\
            <tr><td colspan=2>wide<br>text</td></tr></table><!--EndFragment--></body></html>";
        assert_eq!(
            parse_html_table(html).unwrap(),
            rows(&[&["a & b", "1 2"], &["wide\ntext", ""]])
        );
    }

    #[test]
    fn source_line_breaks_are_blanks_and_nbsp_survives() {
        let html = "<table><tr><td>long\r\n  text\tmore</td><td>&nbsp;&nbsp;ID</td><td>&nbsp;</td>\
            <td><p>one</p><p>two</p></td></tr></table>";
        assert_eq!(
            parse_html_table(html).unwrap(),
            rows(&[&["long text more", "  ID", "", "one\ntwo"]])
        );
    }

    #[test]
    fn spans_fill_the_cells_they_cover_and_unclosed_tags_end_at_the_next() {
        let html = "<TABLE><TR><TD rowspan=2>a<TD>b<TR><TD>c<TR><TH>d<TH>e&#33;&#x21;</TABLE>";
        assert_eq!(
            parse_html_table(html).unwrap(),
            rows(&[&["a", "b"], &["", "c"], &["d", "e!!"]])
        );
    }

    #[test]
    fn no_table_or_no_cell_is_none() {
        assert_eq!(parse_html_table("<p>just text</p>"), None);
        assert_eq!(parse_html_table("<table></table>"), None);
        assert_eq!(parse_html_table(""), None);
    }

    #[test]
    fn a_table_with_other_content_or_a_second_table_is_not_a_table_copy() {
        let t = "<table><tr><td>x</td></tr></table>";
        assert_eq!(
            parse_html_table(&format!("<h1>Title</h1>{t}<p>after</p>")),
            None
        );
        assert_eq!(parse_html_table(&format!("{t}<p>after</p>")), None);
        assert_eq!(parse_html_table(&format!("{t}{t}")), None);
        assert!(parse_html_table(&format!("<p> &nbsp;</p>{t}\n")).is_some());
        assert!(parse_html_table(&format!("a &lt; b {t}")).is_none());
    }

    #[test]
    fn a_nested_table_belongs_to_its_cell() {
        let html =
            "<table><tr><td>x<table><tr><td>inner</td></tr></table>y</td><td>z</td></tr></table>";
        assert_eq!(parse_html_table(html).unwrap(), rows(&[&["xinnery", "z"]]));
    }

    #[test]
    fn a_hostile_table_is_bounded() {
        let big = "<table><tr><td rowspan=1000 colspan=1000>a</td></tr></table>";
        assert_eq!(parse_html_table(big).unwrap().len(), 1000);
        // A thousand one-cell rows and one a thousand wide: a million cells
        // once padded, and several thousand of them past the cap.
        let mut wide = String::from("<table>");
        for _ in 0..5000 {
            wide.push_str("<tr><td>x</td></tr>");
        }
        wide.push_str("<tr><td colspan=1000>y</td></tr></table>");
        assert_eq!(parse_html_table(&wide), None);
        // The same span over and over costs time, not memory.
        let mut spans = String::from("<table><tr>");
        for _ in 0..5000 {
            spans.push_str("<td rowspan=1000 colspan=1000>z</td>");
        }
        spans.push_str("</tr></table>");
        assert_eq!(parse_html_table(&spans), None);
        // Bare ampersands are linear.
        let amps = format!("<table><tr><td>{}</td></tr></table>", "&".repeat(200_000));
        assert_eq!(parse_html_table(&amps).unwrap()[0][0].len(), 200_000);
    }

    #[test]
    fn an_rtf_table_reads_its_cells() {
        let rtf = "{\\rtf1\\ansi\\ansicpg1252{\\fonttbl{\\f0 Arial;}}{\\colortbl;\\red0\\green0\\blue0;}\
            \\trowd\\cellx1000\\cellx2000\\pard\\intbl a\\'e9\\cell \\pard\\intbl {\\b b}\\u8364?\\cell \\row\
            \\trowd\\cellx1000\\pard\\intbl c\\line d\\par\\cell \\row\
            \\pard\\par}";
        assert_eq!(
            parse_rtf_table(rtf).unwrap(),
            rows(&[&["a\u{e9}", "b\u{20ac}"], &["c\nd", ""]])
        );
    }

    #[test]
    fn an_rtf_table_with_other_text_or_none_is_not_a_table_copy() {
        let t = "\\trowd\\cellx1000\\pard\\intbl a\\cell \\row";
        assert!(parse_rtf_table(&format!("{{\\rtf1 {t}\\pard\\par}}")).is_some());
        assert_eq!(
            parse_rtf_table(&format!("{{\\rtf1 before\\par {t}}}")),
            None
        );
        assert_eq!(
            parse_rtf_table(&format!("{{\\rtf1 {t}\\pard after\\par}}")),
            None
        );
        assert_eq!(parse_rtf_table("{\\rtf1 plain\\par}"), None);
        assert_eq!(parse_rtf_table(""), None);
    }

    /// Word, and docxy's own export, repeat the row definition before `\row`.
    #[test]
    fn a_row_definition_repeated_before_row_keeps_the_cells() {
        let rtf = "{\\rtf1\\ansi\\trowd\\cellx1000\\cellx2000\\pard\\intbl a\\cell\\pard\\intbl b\\cell\
            \\pard\\intbl\\trowd\\cellx1000\\cellx2000\\row\
            \\trowd\\cellx1000\\cellx2000\\pard\\intbl c\\cell\\pard\\intbl d\\cell\
            \\pard\\intbl\\trowd\\cellx1000\\cellx2000\\row\\pard\\plain\n}";
        assert_eq!(
            parse_rtf_table(rtf).unwrap(),
            rows(&[&["a", "b"], &["c", "d"]])
        );
    }

    #[test]
    fn an_rtf_with_a_wide_character_after_a_backslash_does_not_panic() {
        assert_eq!(parse_rtf_table("{\\rtf1 \\\u{e9}}"), None);
        let rtf = "{\\rtf1\\trowd\\cellx1000 \\'9\u{e9}x\\cell \\'\u{e9}\\cell \\row}";
        let cells = parse_rtf_table(rtf).unwrap();
        assert_eq!(cells.len(), 1);
        assert!(cells[0][0].ends_with("\u{e9}x") || cells[0][0].contains('x'));
        assert_eq!(parse_rtf_table("{\\rtf1 \\"), None);
        assert_eq!(parse_rtf_table("{\\rtf1 \\'4"), None);
    }

    #[test]
    fn the_rtf_code_page_reads_high_bytes() {
        let rtf = "{\\rtf1\\ansicpg1251\\trowd\\cellx1000\\pard\\intbl \\'cf\\'f0\\cell \\row}";
        assert_eq!(parse_rtf_table(rtf).unwrap(), rows(&[&["\u{41f}\u{440}"]]));
    }

    #[test]
    fn a_hostile_rtf_is_bounded() {
        let mut rtf = String::from("{\\rtf1\\trowd\\cellx1000 ");
        for _ in 0..5000 {
            rtf.push_str("x\\cell \\row ");
        }
        rtf.push_str("y\\cell ");
        for _ in 0..1000 {
            rtf.push_str("z\\cell ");
        }
        rtf.push_str("\\row}");
        assert_eq!(parse_rtf_table(&rtf), None);
        let mut empties = String::from("{\\rtf1\\trowd ");
        for _ in 0..4_100_000 {
            empties.push_str("\\row");
        }
        empties.push('}');
        assert_eq!(parse_rtf_table(&empties), None);
    }
}
