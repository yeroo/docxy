//! Plain text from the small RTF dialect Project writes for task notes.

#[derive(Clone, Copy)]
struct State {
    hidden: bool,
    uc: usize,
    codepage: i32,
}

fn cp1252(byte: u8) -> char {
    const EXT: [char; 32] = [
        '€', '\u{0081}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{008d}', 'Ž',
        '\u{008f}', '\u{0090}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ',
        '\u{009d}', 'ž', 'Ÿ',
    ];
    if (0x80..=0x9f).contains(&byte) {
        EXT[(byte - 0x80) as usize]
    } else {
        char::from(byte)
    }
}

fn push(out: &mut Vec<u16>, c: char) {
    out.extend(c.encode_utf16(&mut [0; 2]).iter().copied());
}

fn skip_fallback(bytes: &[u8], pos: &mut usize, count: usize) -> Result<(), ()> {
    for _ in 0..count {
        if *pos >= bytes.len() {
            return Err(());
        }
        if bytes[*pos] == b'\\' {
            *pos += 1;
            if *pos >= bytes.len() {
                return Err(());
            }
            if bytes[*pos] == b'\'' {
                if *pos + 2 >= bytes.len()
                    || !bytes[*pos + 1].is_ascii_hexdigit()
                    || !bytes[*pos + 2].is_ascii_hexdigit()
                {
                    return Err(());
                }
                *pos += 3;
            } else if !bytes[*pos].is_ascii_alphabetic() {
                *pos += 1;
            } else {
                return Err(());
            }
        } else if !matches!(bytes[*pos], b'{' | b'}' | b'\r' | b'\n') {
            *pos += 1;
        } else {
            return Err(());
        }
    }
    Ok(())
}

fn parse(bytes: &[u8]) -> Result<String, ()> {
    if !bytes.starts_with(b"{\\rtf") {
        return Err(());
    }
    let mut pos = 0;
    let mut stack = Vec::new();
    let mut state = State {
        hidden: false,
        uc: 1,
        codepage: 1252,
    };
    let mut out = Vec::new();
    let mut group_start = false;
    let mut closed = false;
    let mut final_par = false;
    while pos < bytes.len() {
        let b = bytes[pos];
        if closed {
            if !matches!(b, b'\r' | b'\n' | 0) {
                return Err(());
            }
            pos += 1;
            continue;
        }
        match b {
            b'{' => {
                stack.push(state);
                group_start = true;
                pos += 1;
            }
            b'}' => {
                state = stack.pop().ok_or(())?;
                pos += 1;
                group_start = false;
                closed = stack.is_empty();
            }
            b'\r' | b'\n' => pos += 1,
            b'\\' => {
                pos += 1;
                let &next = bytes.get(pos).ok_or(())?;
                if next.is_ascii_alphabetic() {
                    let start = pos;
                    while bytes.get(pos).is_some_and(u8::is_ascii_alphabetic) {
                        pos += 1;
                    }
                    let word = &bytes[start..pos];
                    let mut sign = 1i32;
                    if bytes.get(pos) == Some(&b'-') {
                        sign = -1;
                        pos += 1;
                    }
                    let num_start = pos;
                    while bytes.get(pos).is_some_and(u8::is_ascii_digit) {
                        pos += 1;
                    }
                    let num = if pos > num_start {
                        Some(
                            std::str::from_utf8(&bytes[num_start..pos])
                                .map_err(|_| ())?
                                .parse::<i32>()
                                .map_err(|_| ())?
                                .checked_mul(sign)
                                .ok_or(())?,
                        )
                    } else {
                        if sign == -1 {
                            return Err(());
                        }
                        None
                    };
                    if bytes.get(pos) == Some(&b' ') {
                        pos += 1;
                    }
                    if group_start
                        && matches!(
                            word,
                            b"fonttbl" | b"colortbl" | b"stylesheet" | b"info" | b"generator"
                        )
                    {
                        state.hidden = true;
                    }
                    group_start = false;
                    match word {
                        b"ansicpg" => state.codepage = num.ok_or(())?,
                        b"uc" => state.uc = usize::try_from(num.ok_or(())?).map_err(|_| ())?,
                        b"u" => {
                            let value = num.ok_or(())?;
                            if !(-32768..=65535).contains(&value) {
                                return Err(());
                            }
                            if !state.hidden {
                                out.push(value as u16);
                                final_par = false;
                            }
                            skip_fallback(bytes, &mut pos, state.uc)?;
                        }
                        b"par" | b"line" if !state.hidden => {
                            out.extend([b'\r' as u16, b'\n' as u16]);
                            final_par = word == b"par";
                        }
                        b"tab" | b"emdash" | b"endash" | b"bullet" | b"lquote" | b"rquote"
                        | b"ldblquote" | b"rdblquote"
                            if !state.hidden =>
                        {
                            let c = match word {
                                b"tab" => '\t',
                                b"emdash" => '—',
                                b"endash" => '–',
                                b"bullet" => '•',
                                b"lquote" => '‘',
                                b"rquote" => '’',
                                b"ldblquote" => '“',
                                _ => '”',
                            };
                            push(&mut out, c);
                            final_par = false;
                        }
                        _ => {}
                    }
                } else {
                    pos += 1;
                    match next {
                        b'*' if group_start => state.hidden = true,
                        b'\'' => {
                            let digits = bytes.get(pos..pos + 2).ok_or(())?;
                            let hex = std::str::from_utf8(digits).map_err(|_| ())?;
                            let value = u8::from_str_radix(hex, 16).map_err(|_| ())?;
                            pos += 2;
                            if !state.hidden {
                                if state.codepage != 1252 {
                                    return Err(());
                                }
                                push(&mut out, cp1252(value));
                                final_par = false;
                            }
                        }
                        b'\\' | b'{' | b'}' | b'~' | b'_' | b'-' if !state.hidden => {
                            let c = match next {
                                b'~' => '\u{a0}',
                                b'_' => '\u{2011}',
                                b'-' => '\u{ad}',
                                _ => next as char,
                            };
                            push(&mut out, c);
                            final_par = false;
                        }
                        b'*' | b'\\' | b'{' | b'}' | b'~' | b'_' | b'-' => {}
                        _ => return Err(()),
                    }
                    group_start = false;
                }
            }
            0 => return Err(()),
            _ => {
                if b >= 0x80 {
                    return Err(());
                }
                if !state.hidden {
                    push(&mut out, b as char);
                    final_par = false;
                }
                pos += 1;
                group_start = false;
            }
        }
    }
    if !closed {
        return Err(());
    }
    // RichEdit ends every note with a paragraph marker, which Project's XML
    // export does not include as part of Notes.
    if final_par {
        out.truncate(out.len() - 2);
    }
    String::from_utf16(&out).map_err(|_| ())
}

pub(crate) fn plain_text(bytes: &[u8], uid: u32) -> Result<String, String> {
    parse(bytes).map_err(|_| format!("invalid notes for UID {uid}"))
}

#[cfg(test)]
mod tests {
    use super::plain_text;

    #[test]
    fn rich_edit_note() {
        let rtf = b"{\\rtf1\\ansi\\ansicpg1252{\\fonttbl{\\f0 Arial;}}\r\n{\\*\\generator RichEdit}\\uc1 First line.\\par\r\nSecond \\emdash  \\u10003? and \\u171?quotes\\u187?.\\par\r\n}\r\n\0";
        assert_eq!(
            plain_text(rtf, 8).unwrap(),
            "First line.\r\nSecond — ✓ and «quotes»."
        );
    }

    #[test]
    fn symbols_escapes_and_codepage() {
        let rtf = br"{\rtf1\ansi\ansicpg1252 a\'e9\tab\{x\}\\\~\_\-\endash\bullet\lquote\rquote\ldblquote\rdblquote}";
        assert_eq!(
            plain_text(rtf, 1).unwrap(),
            "aé\t{x}\\\u{a0}\u{2011}\u{ad}–•‘’“”"
        );
    }

    #[test]
    fn line_break_is_kept_when_it_is_the_note_content() {
        assert_eq!(plain_text(br"{\rtf1 A\line}", 1).unwrap(), "A\r\n");
        assert_eq!(plain_text(br"{\rtf1 A\line\par}", 1).unwrap(), "A\r\n");
    }

    #[test]
    fn unicode_fallback_count_and_signed_units() {
        assert_eq!(
            plain_text(br"{\rtf1\uc2\u233?X\uc1\u-1?}", 1).unwrap(),
            "é\u{ffff}"
        );
        assert_eq!(
            plain_text(br"{\rtf1\uc1\u55357?\u56832?}", 1).unwrap(),
            "😀"
        );
    }

    #[test]
    fn malformed_or_unsupported_is_refused() {
        for rtf in [
            br"plain".as_slice(),
            br"{\rtf1 missing".as_slice(),
            br"{\rtf1\'zz}".as_slice(),
            br"{\rtf1\ansicpg932\'82}".as_slice(),
            br"{\rtf1 ok}garbage".as_slice(),
        ] {
            assert_eq!(plain_text(rtf, 7), Err("invalid notes for UID 7".into()));
        }
    }
}
