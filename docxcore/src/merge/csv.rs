//! A mail-merge recipient list read from a delimited text file (`.csv` /
//! `.txt`), as Word's "Use an Existing List…" reads one: the first row names
//! the columns, every other row is a recipient.
//!
//! Modelled on `gridcore::frame::parse_delimited`, which docxcore cannot
//! depend on.

/// The attached recipient list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Recipients {
    /// Column names, from the header row (blank names become `ColumnN`).
    pub headers: Vec<String>,
    /// One row per recipient, each exactly `headers.len()` long.
    pub rows: Vec<Vec<String>>,
    /// Whether each row takes part in the merge (Edit Recipient List's
    /// check boxes); all true when read.
    pub included: Vec<bool>,
    /// The file the list was read from, when it came from one.
    pub source: Option<String>,
}

impl Recipients {
    /// Read a delimited file. The delimiter (`,`, `;` or tab) is sniffed from
    /// the header row; quotes follow RFC 4180 (`""` is a quote, and a quoted
    /// field may hold the delimiter or a line break). A UTF-8 or UTF-16LE
    /// byte-order mark is dropped; bytes that are not UTF-8 are read as
    /// Windows-1252, as an ANSI export from Excel is. Blank lines are skipped,
    /// short rows are padded with empty values and long rows cut to the
    /// header. Only a header gives zero recipients; no header is an error.
    pub fn parse_csv(bytes: &[u8]) -> Result<Recipients, String> {
        let text = decode(bytes);
        let delim = sniff_delimiter(&text);
        let mut records = parse_delimited(&text, delim)
            .into_iter()
            .filter(|r| !(r.len() == 1 && r[0].trim().is_empty()));
        let headers: Vec<String> = records
            .next()
            .ok_or_else(|| "no header row".to_string())?
            .into_iter()
            .enumerate()
            .map(|(i, h)| {
                let h = h.trim();
                if h.is_empty() {
                    format!("Column{}", i + 1)
                } else {
                    h.to_string()
                }
            })
            .collect();
        let rows: Vec<Vec<String>> = records
            .map(|mut r| {
                r.resize(headers.len(), String::new());
                r
            })
            .collect();
        Ok(Recipients {
            included: vec![true; rows.len()],
            headers,
            rows,
            source: None,
        })
    }

    /// The column named `name`: case-insensitive, and a space matches an
    /// underscore (Word writes `MERGEFIELD First_Name` for a "First Name"
    /// column).
    pub fn column(&self, name: &str) -> Option<usize> {
        let want = name_key(name);
        self.headers.iter().position(|h| name_key(h) == want)
    }

    /// Row `row`'s value in column `col`, empty when out of range.
    pub fn value(&self, row: usize, col: usize) -> &str {
        self.rows
            .get(row)
            .and_then(|r| r.get(col))
            .map_or("", String::as_str)
    }

    /// The rows that take part in a merge, in order.
    pub fn included_rows(&self) -> Vec<usize> {
        (0..self.rows.len())
            .filter(|&r| self.included.get(r).copied().unwrap_or(true))
            .collect()
    }
}

/// A column name for comparison: lowercase, with spaces and underscores
/// equal and surrounding space ignored.
pub(crate) fn name_key(name: &str) -> String {
    name.trim()
        .chars()
        .map(|c| if c == '_' { ' ' } else { c })
        .flat_map(char::to_lowercase)
        .collect()
}

/// Windows-1252's 0x80–0x9F; the five holes map to the C1 control of the
/// same number, as Windows does.
const CP1252_HIGH: [char; 32] = [
    '\u{20AC}', '\u{0081}', '\u{201A}', '\u{0192}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{02C6}', '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{008D}', '\u{017D}', '\u{008F}',
    '\u{0090}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{02DC}', '\u{2122}', '\u{0161}', '\u{203A}', '\u{0153}', '\u{009D}', '\u{017E}', '\u{0178}',
];

fn decode(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(b"\xEF\xBB\xBF") {
        return String::from_utf8_lossy(rest).into_owned();
    }
    if let Some(rest) = bytes.strip_prefix(b"\xFF\xFE") {
        let units = rest
            .chunks(2)
            .map(|p| u16::from_le_bytes([p[0], p.get(1).copied().unwrap_or(0)]));
        return char::decode_utf16(units)
            .map(|r| r.unwrap_or('\u{FFFD}'))
            .collect();
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => bytes
            .iter()
            .map(|&b| match b {
                0x80..=0x9F => CP1252_HIGH[(b - 0x80) as usize],
                _ => b as char,
            })
            .collect(),
    }
}

/// The delimiter the header row uses most: `,`, `;` or tab (`,` on a tie or
/// when there is none).
fn sniff_delimiter(text: &str) -> char {
    let mut counts = [0usize; 3];
    let mut in_quotes = false;
    for ch in text.chars() {
        match ch {
            '"' => in_quotes = !in_quotes,
            '\n' if !in_quotes => break,
            ',' if !in_quotes => counts[0] += 1,
            ';' if !in_quotes => counts[1] += 1,
            '\t' if !in_quotes => counts[2] += 1,
            _ => {}
        }
    }
    let best = (0..3).rev().max_by_key(|&i| counts[i]).unwrap_or(0);
    if counts[best] == 0 {
        ','
    } else {
        [',', ';', '\t'][best]
    }
}

fn parse_delimited(text: &str, delim: char) -> Vec<Vec<String>> {
    let mut records = Vec::new();
    let mut record: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if in_quotes {
            match ch {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => in_quotes = false,
                _ => field.push(ch),
            }
        } else {
            match ch {
                '"' => in_quotes = true,
                c if c == delim => record.push(std::mem::take(&mut field)),
                '\r' if chars.peek() == Some(&'\n') => {}
                '\r' | '\n' => {
                    record.push(std::mem::take(&mut field));
                    records.push(std::mem::take(&mut record));
                }
                _ => field.push(ch),
            }
        }
    }
    if !field.is_empty() || !record.is_empty() {
        record.push(field);
        records.push(record);
    }
    records
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Recipients {
        Recipients::parse_csv(s.as_bytes()).unwrap()
    }

    #[test]
    fn header_and_rows() {
        let r = parse("First,Last\nJane,Doe\nJohn,Smith\n");
        assert_eq!(r.headers, ["First", "Last"]);
        assert_eq!(r.rows, [["Jane", "Doe"], ["John", "Smith"]]);
        assert_eq!(r.included, [true, true]);
        assert_eq!(r.included_rows(), [0, 1]);
    }

    #[test]
    fn utf8_bom_is_dropped() {
        let r = Recipients::parse_csv(b"\xEF\xBB\xBFName\nZo\xC3\xAB\n").unwrap();
        assert_eq!(r.headers, ["Name"]);
        assert_eq!(r.rows, [["Zoë"]]);
    }

    #[test]
    fn utf16le_bom_and_windows_1252_fallback() {
        let mut bytes = vec![0xFF, 0xFE];
        for u in "N\nA\n".encode_utf16() {
            bytes.extend(u.to_le_bytes());
        }
        assert_eq!(Recipients::parse_csv(&bytes).unwrap().rows, [["A"]]);
        // 0x93/0x94 are curly quotes, 0xE9 é: not UTF-8.
        let r = Recipients::parse_csv(b"N\n\x93Ren\xE9\x94\n").unwrap();
        assert_eq!(r.rows, [["\u{201C}René\u{201D}"]]);
    }

    #[test]
    fn delimiter_is_sniffed() {
        assert_eq!(parse("a;b\n1;2\n").rows, [["1", "2"]]);
        assert_eq!(parse("a\tb\n1\t2\n").rows, [["1", "2"]]);
        // A comma inside the semicolon file's values is data.
        assert_eq!(parse("a;b\n1,5;2\n").rows, [["1,5", "2"]]);
    }

    #[test]
    fn quotes_doubled_quotes_and_embedded_newlines() {
        let r = parse("Name,Note\n\"Doe, Jane\",\"She said \"\"hi\"\"\nthen left\"\n");
        assert_eq!(r.rows, [["Doe, Jane", "She said \"hi\"\nthen left"]]);
    }

    #[test]
    fn crlf_and_lone_cr_end_rows() {
        assert_eq!(
            parse("a,b\r\n1,2\r\n3,4\r\n").rows,
            [["1", "2"], ["3", "4"]]
        );
        assert_eq!(parse("a\r1\r2").rows, [["1"], ["2"]]);
    }

    #[test]
    fn ragged_rows_are_padded_or_cut() {
        let r = parse("a,b,c\n1\n1,2,3,4\n");
        assert_eq!(r.rows, [["1", "", ""], ["1", "2", "3"]]);
    }

    #[test]
    fn blank_lines_are_skipped() {
        assert_eq!(parse("a\n\n1\n\n\n").rows, [["1"]]);
        assert_eq!(parse("\n\na\n1").headers, ["a"]);
    }

    #[test]
    fn header_only_is_zero_rows_and_nothing_is_an_error() {
        let r = parse("First,Last\n");
        assert_eq!(r.headers, ["First", "Last"]);
        assert!(r.rows.is_empty());
        assert_eq!(
            Recipients::parse_csv(b"").unwrap_err(),
            "no header row".to_string()
        );
        assert!(Recipients::parse_csv(b"\r\n\n  \n").is_err());
    }

    #[test]
    fn blank_header_names_and_column_lookup() {
        let r = parse(" First Name ,,Zip\nJane,x,1\n");
        assert_eq!(r.headers, ["First Name", "Column2", "Zip"]);
        assert_eq!(r.column("first_name"), Some(0));
        assert_eq!(r.column("FIRST NAME"), Some(0));
        assert_eq!(r.column("zip"), Some(2));
        assert_eq!(r.column("City"), None);
        assert_eq!(r.value(0, 2), "1");
        assert_eq!(r.value(5, 0), "");
    }
}
