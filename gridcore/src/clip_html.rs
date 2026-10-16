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
    let mut out = String::from("<table border=0 cellpadding=0 cellspacing=0>");
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
                        format!(" style=\"{css}\"")
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
                        out.push_str(&format!("<td style=\"{css}\"></td>"));
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
    cells: usize,
}

impl Grid {
    fn ensure(&mut self, r: usize, c: usize) -> bool {
        if r >= self.rows.len() || c >= self.rows[r].len() {
            let grow = (r + 1).saturating_sub(self.rows.len())
                + c.saturating_sub(self.rows.get(r).map_or(0, Vec::len))
                + 1;
            self.cells += grow;
            if self.cells > MAX_CELLS {
                return false;
            }
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

    fn finish(self) -> Option<Vec<Vec<String>>> {
        let width = self.rows.iter().map(Vec::len).max().unwrap_or(0);
        if width == 0 {
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
        "nbsp" => ' ',
        _ => return None,
    })
}

/// `text` with entities decoded.
fn decode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let decoded = rest[1..]
            .find(';')
            .filter(|&n| n <= 10)
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

/// A cell's text with HTML whitespace collapsed: a run of blanks is one
/// space, the cell's ends are trimmed; line breaks were already LF.
fn collapse(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut blank = false;
    for c in raw.chars() {
        if c == '\n' {
            while out.ends_with(' ') {
                out.pop();
            }
            out.push('\n');
            blank = true;
        } else if c.is_whitespace() {
            if !blank {
                out.push(' ');
                blank = true;
            }
        } else {
            out.push(c);
            blank = false;
        }
    }
    out.trim_matches([' ', '\n']).to_string()
}

/// The cell texts of the first `<table>` in `html`, rows by columns (short
/// rows padded; a `colspan`/`rowspan` fills the cells it covers with empty
/// text), or `None` for no table or no cell. Entities are decoded and a
/// `<br>` is a line feed. Text before the table (a CF_HTML header, `<html>`
/// and `<style>`) is skipped, and unclosed `<td>`, `<tr>` and `<table>` end
/// at the next one.
#[allow(unused_assignments)]
pub fn parse_html_table(html: &str) -> Option<Vec<Vec<String>>> {
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<table")?;
    let mut grid = Grid::default();
    let mut row: Option<usize> = None;
    let mut col = 0usize;
    let mut cell: Option<(String, usize, usize)> = None;
    let mut next_row = 0usize;
    let mut i = start;
    let mut depth = 0usize;
    let bytes = html.as_bytes();
    // Text up to the next tag, tag by tag.
    macro_rules! close_cell {
        () => {
            if let Some((text, cs, rs)) = cell.take() {
                let r = row?;
                col = grid.place(r, col, collapse(&decode(&text)), cs, rs)?;
            }
        };
    }
    while i < bytes.len() {
        if bytes[i] != b'<' {
            let end = html[i..].find('<').map_or(html.len(), |n| i + n);
            if let Some((text, ..)) = cell.as_mut() {
                text.push_str(&html[i..end]);
            }
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
            ("table", false) => depth += 1,
            ("table", true) => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    close_cell!();
                    break;
                }
            }
            ("style" | "script", false) => {
                let close = format!("</{name}");
                i = lower[i..].find(&close).map_or(html.len(), |n| i + n);
            }
            // A nested table's cells belong to the outer cell's text: only
            // the outermost table's own rows and cells make the grid.
            _ if depth > 1 => {}
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
            ("br", _) => {
                if let Some((text, ..)) = cell.as_mut() {
                    text.push('\n');
                }
            }
            ("p" | "div", true) => {
                if let Some((text, ..)) = cell.as_mut() {
                    if !text.is_empty() && !text.ends_with('\n') {
                        text.push('\n');
                    }
                }
            }
            _ => {}
        }
    }
    close_cell!();
    grid.finish()
}

/// The texts of the bold cells (`font-weight` 700 or bold on a `<td>` or
/// `<th>`) of the first table in `html`, in order: what a pasted table would
/// show in bold. For tests of what a copy puts on the clipboard.
pub fn html_bold_cells(html: &str) -> Vec<String> {
    let lower = html.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut at = lower.find("<table").unwrap_or(lower.len());
    while let Some(i) = lower[at..].find("<td").map(|n| at + n) {
        let Some(open_end) = lower[i..].find('>').map(|n| i + n) else {
            break;
        };
        let close = lower[open_end..]
            .find("</td")
            .map_or(lower.len(), |n| open_end + n);
        let tag = &lower[i..open_end];
        if tag.contains("font-weight:700") || tag.contains("font-weight:bold") {
            let cell = format!(
                "<table><tr><td>{}</td></tr></table>",
                &html[open_end + 1..close]
            );
            if let Some(rows) = parse_html_table(&cell) {
                out.push(rows[0][0].clone());
            }
        }
        at = close;
    }
    out
}

/// The character of byte `b` in Windows-1252.
fn windows_1252(b: u8) -> char {
    const HIGH: [char; 32] = [
        '\u{20ac}', '\u{81}', '\u{201a}', '\u{192}', '\u{201e}', '\u{2026}', '\u{2020}',
        '\u{2021}', '\u{2c6}', '\u{2030}', '\u{160}', '\u{2039}', '\u{152}', '\u{8d}', '\u{17d}',
        '\u{8f}', '\u{90}', '\u{2018}', '\u{2019}', '\u{201c}', '\u{201d}', '\u{2022}', '\u{2013}',
        '\u{2014}', '\u{2dc}', '\u{2122}', '\u{161}', '\u{203a}', '\u{153}', '\u{9d}', '\u{17e}',
        '\u{178}',
    ];
    match b {
        0x80..=0x9f => HIGH[(b - 0x80) as usize],
        _ => b as char,
    }
}

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

/// The cell texts of the first table in `rtf` (a `\trowd` row with its
/// `\cell`s and `\row`), or `None` when the RTF holds no table. Control words
/// and groups are dropped, `\'hh` is read as Windows-1252, `\uN` as UTF-16 (the
/// fallback character after it skipped), `\par` and `\line` as a line feed
/// inside the cell. Rows are padded to the widest.
pub fn parse_rtf_table(rtf: &str) -> Option<Vec<Vec<String>>> {
    let b = rtf.as_bytes();
    let mut i = 0;
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_table = false;
    let mut cells = 0usize;
    let mut skip_depth: Option<usize> = None;
    let mut depth = 0usize;
    let mut uc = 1usize;
    let mut uc_skip = 0usize;
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
                let Some(&next) = b.get(i) else {
                    break;
                };
                if !next.is_ascii_alphabetic() {
                    i += 1;
                    let skipping = skip_depth.is_some();
                    match next {
                        b'\'' => {
                            let hex = rtf
                                .get(i..i + 2)
                                .and_then(|h| u8::from_str_radix(h, 16).ok());
                            i += 2.min(b.len() - i);
                            if uc_skip > 0 {
                                uc_skip -= 1;
                            } else if let (Some(byte), false) = (hex, skipping) {
                                cur.push(windows_1252(byte));
                            }
                        }
                        b'\\' | b'{' | b'}' if !skipping => {
                            if uc_skip > 0 {
                                uc_skip -= 1;
                            } else {
                                cur.push(next as char);
                            }
                        }
                        b'\n' | b'\r' if !skipping => cur.push('\n'),
                        b'~' if !skipping => cur.push(' '),
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
                    "trowd" => {
                        in_table = true;
                        cur.clear();
                        row.clear();
                    }
                    "cell" | "nestcell" if in_table => {
                        row.push(cur.trim_matches(['\n', ' ']).to_string());
                        cur.clear();
                        cells += 1;
                        if cells > MAX_CELLS {
                            return None;
                        }
                    }
                    "row" | "nestrow" if in_table => {
                        if !cur.trim().is_empty() {
                            row.push(cur.trim_matches(['\n', ' ']).to_string());
                        }
                        cur.clear();
                        rows.push(std::mem::take(&mut row));
                    }
                    "par" | "line" => cur.push('\n'),
                    "tab" => cur.push(' '),
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
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    if !in_table || rows.is_empty() || width == 0 {
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

    fn sheet_with(cells: &[(u32, u32, Cell)], styles: &mut Styles) -> Sheet {
        let _ = styles;
        let mut sh = Sheet {
            name: "S".into(),
            ..Default::default()
        };
        for (r, c, cell) in cells {
            sh.set_cell(*r, *c, cell.clone());
        }
        sh
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
        let sh = sheet_with(&[(0, 0, b), (0, 1, Cell::number(1.5))], &mut styles);
        let html = html_table(&sh, &styles, false, &[0, 1], &[0, 1]);
        assert!(html.starts_with("<table"));
        assert!(
            html.contains("font-weight:700;color:#ff0000;background:#00ff00;text-align:center;")
        );
        assert!(html.contains("a&lt;b&gt; &amp; &quot;c&quot;<br>d"));
        assert!(
            html.contains("<td style=\"text-align:right;\">1.5</td>"),
            "{html}"
        );
        // Two rows, two cells each, the empty ones present.
        assert_eq!(html.matches("<tr>").count(), 2);
        assert_eq!(html.matches("<td").count(), 4);
        assert_eq!(parse_html_table(&html).unwrap()[0][0], "a<b> & \"c\"\nd");
    }

    #[test]
    fn the_bold_cells_are_listed() {
        let html = "<table><tr><td style=\"font-weight:700;\">a&amp;b</td><td>c</td>\
            <td style='font-weight:bold'>d</td></tr></table>";
        assert_eq!(html_bold_cells(html), vec!["a&b", "d"]);
        assert!(html_bold_cells("<p>x</p>").is_empty());
    }

    /// The TSV beside the table carries a `'` for text that reads as a number
    /// (`entry::copy_field`); the table does not, since another app would show
    /// it. A docxy paste takes the table over the text, so "007" copied as
    /// text pastes back as the number 7: the cost of HTML winning (#1248).
    #[test]
    fn a_numeric_looking_text_has_no_apostrophe_in_the_table() {
        let mut styles = Styles::default();
        let sh = sheet_with(&[(0, 0, Cell::text("007"))], &mut styles);
        let html = html_table(&sh, &styles, false, &[0], &[0]);
        assert!(html.contains(">007<") && !html.contains("'007"));
        assert_eq!(parse_html_table(&html).unwrap(), vec![vec!["007"]]);
    }

    #[test]
    fn a_formula_shows_only_its_result() {
        let mut styles = Styles::default();
        let mut cell = Cell::number(3.0);
        cell.formula = Some("1+2".into());
        let sh = sheet_with(&[(0, 0, cell)], &mut styles);
        let html = html_table(&sh, &styles, false, &[0], &[0]);
        assert!(html.contains(">3<"));
        assert!(!html.contains("1+2"));
    }

    #[test]
    fn a_parsed_table_has_its_cells_by_position() {
        let html = "Version:0.9\r\nStartHTML:0000\r\n<html><head><style>td{x:y}</style></head><body>\
            <!--StartFragment--><table><tr><td class=xl65 style='x:y'>a &amp; b</td><td>1&nbsp;2</td></tr>\
            <tr><td colspan=2>wide<br>text</td></tr></table><!--EndFragment--></body></html>";
        assert_eq!(
            parse_html_table(html).unwrap(),
            vec![vec!["a & b", "1 2"], vec!["wide\ntext", ""]]
        );
    }

    #[test]
    fn spans_fill_the_cells_they_cover_and_unclosed_tags_end_at_the_next() {
        let html = "<TABLE><TR><TD rowspan=2>a<TD>b<TR><TD>c<TR><TH>d<TH>e&#33;&#x21;</TABLE>";
        assert_eq!(
            parse_html_table(html).unwrap(),
            vec![vec!["a", "b"], vec!["", "c"], vec!["d", "e!!"]]
        );
    }

    #[test]
    fn no_table_or_no_cell_is_none() {
        assert_eq!(parse_html_table("<p>just text</p>"), None);
        assert_eq!(parse_html_table("<table></table>"), None);
        assert_eq!(parse_html_table(""), None);
    }

    #[test]
    fn a_nested_table_belongs_to_its_cell_and_a_huge_span_is_bounded() {
        let html =
            "<table><tr><td>x<table><tr><td>inner</td></tr></table>y</td><td>z</td></tr></table>";
        assert_eq!(parse_html_table(html).unwrap(), vec![vec!["xinnery", "z"]]);
        let big = "<table><tr><td rowspan=1000 colspan=1000>a</td></tr></table>";
        assert_eq!(parse_html_table(big).unwrap().len(), 1000);
    }

    #[test]
    fn an_rtf_table_reads_its_cells() {
        let rtf = "{\\rtf1\\ansi{\\fonttbl{\\f0 Arial;}}{\\colortbl;\\red0\\green0\\blue0;}\
            before\\par\
            \\trowd\\cellx1000\\cellx2000\\pard\\intbl a\\'e9\\cell \\pard\\intbl {\\b b}\\u8364?\\cell \\row\
            \\trowd\\cellx1000\\pard\\intbl c\\line d\\par\\cell \\row\
            \\pard after\\par}";
        assert_eq!(
            parse_rtf_table(rtf).unwrap(),
            vec![vec!["a\u{e9}", "b\u{20ac}"], vec!["c\nd", ""]]
        );
    }

    #[test]
    fn an_rtf_without_a_table_is_none() {
        assert_eq!(parse_rtf_table("{\\rtf1 plain\\par}"), None);
        assert_eq!(parse_rtf_table(""), None);
    }
}
