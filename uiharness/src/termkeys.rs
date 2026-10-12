//! What a terminal receives when a key is pressed (#1204).
//!
//! The suite's `real-key` names a key (`ctrl+a`, `shift+tab`, `f2`); a
//! terminal editor is typed into by writing what a terminal would write for
//! it. Two encodings, one per kind of terminal the editor reads:
//!
//! - [`vt_bytes`]: the bytes an xterm-style terminal sends on unix, which
//!   crossterm's unix parser reads back (`0x01..=0x1A` is Ctrl+letter, `0x7F`
//!   Backspace, `ESC x` in one write Alt+x, a lone `ESC` Escape).
//! - [`win32_input`]: on Windows crossterm reads console *input records*, and
//!   conhost makes those from what reaches the pseudo-console. Plain VT bytes
//!   leave conhost to guess (`0x08`: Backspace or Ctrl+H? a lone `ESC`: the
//!   Escape key or the start of a sequence?), so Windows gets the
//!   win32-input-mode sequence Windows Terminal sends,
//!   `ESC [ Vk ; Sc ; Uc ; Kd ; Cs ; Rc _`, once down and once up, which
//!   conhost turns into exactly that key event.
//!
//! [`sgr_click`] is a left click in the SGR mouse encoding, which both
//! crossterm (unix) and conhost (Windows) read.
//!
//! Pure: every name and every byte is a unit test below.

/// A key, as named by a script.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Code {
    Char(char),
    Enter,
    Escape,
    Tab,
    Backspace,
    Delete,
    Insert,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    F(u8),
}

/// A key and the modifiers held with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key {
    pub code: Code,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Key {
    fn plain(code: Code) -> Key {
        Key {
            code,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }
}

/// The key names a script may use, for a refusal.
pub const KEY_NAMES: &str = "a character, enter, escape, tab, backspace, delete, insert, \
     left, right, up, down, home, end, pageup, pagedown, space, f1-f12, \
     with ctrl+, alt+ or shift+ before it";

/// Parse a key name as the suite's `real-key` spells it: modifiers joined by
/// `+` (or `-`) before the key, case-insensitive.
pub fn parse_key(name: &str) -> Result<Key, String> {
    let name = name.trim();
    let refuse = || format!("'{name}' is not a key a terminal can send; keys are {KEY_NAMES}");
    if name.is_empty() {
        return Err(refuse());
    }
    // A key that is itself `+` or `-` ends the name after its separator
    // (`ctrl++`); otherwise the last separator splits modifiers from key.
    let is_sep = |c: char| c == '+' || c == '-';
    let (mods, last) =
        if name.len() > 2 && name.ends_with(is_sep) && name[..name.len() - 1].ends_with(is_sep) {
            (&name[..name.len() - 2], &name[name.len() - 1..])
        } else {
            match name.rfind(is_sep) {
                Some(i) if i > 0 && i + 1 < name.len() => (&name[..i], &name[i + 1..]),
                Some(i) if i > 0 => return Err(refuse()),
                _ => ("", name),
            }
        };
    let mut key = Key::plain(Code::Char(' '));
    for m in mods.split(['+', '-']).filter(|m| !m.is_empty()) {
        match m.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => key.ctrl = true,
            "alt" => key.alt = true,
            "shift" => key.shift = true,
            _ => return Err(refuse()),
        }
    }
    let lower = last.to_ascii_lowercase();
    key.code = match lower.as_str() {
        "enter" | "return" => Code::Enter,
        "escape" | "esc" => Code::Escape,
        "tab" => Code::Tab,
        "backspace" => Code::Backspace,
        "delete" | "del" => Code::Delete,
        "insert" => Code::Insert,
        "left" => Code::Left,
        "right" => Code::Right,
        "up" => Code::Up,
        "down" => Code::Down,
        "home" => Code::Home,
        "end" => Code::End,
        "pageup" => Code::PageUp,
        "pagedown" => Code::PageDown,
        "space" => Code::Char(' '),
        f if f.len() >= 2 && f.starts_with('f') && f[1..].parse::<u8>().is_ok() => {
            let n: u8 = f[1..].parse().unwrap_or(0);
            if !(1..=12).contains(&n) {
                return Err(refuse());
            }
            Code::F(n)
        }
        _ => {
            let mut chars = last.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => {
                    // shift+a is the character `A`; with Ctrl or Alt the
                    // letter is the key, named in lower case.
                    if c.is_ascii_alphabetic() {
                        Code::Char(if key.ctrl || key.alt {
                            c.to_ascii_lowercase()
                        } else if key.shift {
                            key.shift = false;
                            c.to_ascii_uppercase()
                        } else {
                            c
                        })
                    } else {
                        Code::Char(c)
                    }
                }
                _ => return Err(refuse()),
            }
        }
    };
    Ok(key)
}

/// One key per character of `text`, as a person types it: `\n` (or `\r`) is
/// Enter and `\t` Tab.
pub fn typed(text: &str) -> Vec<Key> {
    text.chars()
        .map(|c| match c {
            '\n' | '\r' => Key::plain(Code::Enter),
            '\t' => Key::plain(Code::Tab),
            c => Key::plain(Code::Char(c)),
        })
        .collect()
}

/// The xterm modifier parameter: 1 plus shift 1, alt 2, ctrl 4.
fn xterm_mod(k: &Key) -> u8 {
    1 + u8::from(k.shift) + 2 * u8::from(k.alt) + 4 * u8::from(k.ctrl)
}

/// What a unix terminal writes for `k`.
pub fn vt_bytes(k: &Key) -> Result<Vec<u8>, String> {
    let m = xterm_mod(k);
    let csi = |end: &str| -> Vec<u8> {
        if m == 1 {
            format!("\x1b[{end}").into_bytes()
        } else {
            format!("\x1b[1;{m}{end}").into_bytes()
        }
    };
    let tilde = |n: u8| -> Vec<u8> {
        if m == 1 {
            format!("\x1b[{n}~").into_bytes()
        } else {
            format!("\x1b[{n};{m}~").into_bytes()
        }
    };
    let alt = |mut b: Vec<u8>| -> Vec<u8> {
        if k.alt {
            b.insert(0, 0x1b);
        }
        b
    };
    let cannot = || format!("a unix terminal has no byte sequence for {k:?}");
    Ok(match k.code {
        Code::Left => csi("D"),
        Code::Right => csi("C"),
        Code::Up => csi("A"),
        Code::Down => csi("B"),
        Code::Home => csi("H"),
        Code::End => csi("F"),
        Code::Insert => tilde(2),
        Code::Delete => tilde(3),
        Code::PageUp => tilde(5),
        Code::PageDown => tilde(6),
        Code::F(n @ 1..=4) => {
            let end = (b'P' + n - 1) as char;
            if m == 1 {
                format!("\x1bO{end}").into_bytes()
            } else {
                format!("\x1b[1;{m}{end}").into_bytes()
            }
        }
        Code::F(n) => tilde(match n {
            5 => 15,
            6 => 17,
            7 => 18,
            8 => 19,
            9 => 20,
            10 => 21,
            11 => 23,
            12 => 24,
            _ => return Err(cannot()),
        }),
        Code::Tab if k.shift && !k.ctrl && !k.alt => b"\x1b[Z".to_vec(),
        Code::Tab if !k.ctrl && !k.shift => alt(b"\t".to_vec()),
        Code::Enter if !k.ctrl && !k.shift => alt(b"\r".to_vec()),
        Code::Escape if !k.ctrl && !k.shift => alt(b"\x1b".to_vec()),
        Code::Backspace if !k.ctrl && !k.shift => alt(b"\x7f".to_vec()),
        Code::Char(c) if k.ctrl => {
            if k.shift {
                return Err(cannot());
            }
            let b = match c {
                'a'..='z' => c as u8 & 0x1f,
                ' ' | '@' => 0,
                _ => return Err(cannot()),
            };
            alt(vec![b])
        }
        Code::Char(c) if !k.shift => {
            let mut buf = [0u8; 4];
            alt(c.encode_utf8(&mut buf).as_bytes().to_vec())
        }
        _ => return Err(cannot()),
    })
}

const SHIFT_PRESSED: u32 = 0x10;
const LEFT_CTRL_PRESSED: u32 = 0x08;
const LEFT_ALT_PRESSED: u32 = 0x02;
const ENHANCED_KEY: u32 = 0x100;

/// The US-layout virtual key, set-1 scan code and whether shift makes it,
/// for a printable character. `None` for a character no US key types: it
/// goes as a bare Unicode record (virtual key 0), as a packet does.
fn char_key(c: char) -> Option<(u16, u16, bool)> {
    const ROW_DIGITS: &[u16] = &[0x0B, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A];
    const LETTER_SC: [u16; 26] = [
        0x1E, 0x30, 0x2E, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17, 0x24, 0x25, 0x26, 0x32, 0x31, 0x18,
        0x19, 0x10, 0x13, 0x1F, 0x14, 0x16, 0x2F, 0x11, 0x2D, 0x15, 0x2C,
    ];
    let punct = |base: char| -> Option<(u16, u16)> {
        Some(match base {
            ';' => (0xBA, 0x27),
            '=' => (0xBB, 0x0D),
            ',' => (0xBC, 0x33),
            '-' => (0xBD, 0x0C),
            '.' => (0xBE, 0x34),
            '/' => (0xBF, 0x35),
            '`' => (0xC0, 0x29),
            '[' => (0xDB, 0x1A),
            '\\' => (0xDC, 0x2B),
            ']' => (0xDD, 0x1B),
            '\'' => (0xDE, 0x28),
            _ => return None,
        })
    };
    match c {
        'a'..='z' => Some((
            c.to_ascii_uppercase() as u16,
            LETTER_SC[(c as u8 - b'a') as usize],
            false,
        )),
        'A'..='Z' => Some((c as u16, LETTER_SC[(c as u8 - b'A') as usize], true)),
        '0'..='9' => Some((c as u16, ROW_DIGITS[(c as u8 - b'0') as usize], false)),
        ' ' => Some((0x20, 0x39, false)),
        _ => {
            if let Some((vk, sc)) = punct(c) {
                return Some((vk, sc, false));
            }
            let base = match c {
                '!' => '1',
                '@' => '2',
                '#' => '3',
                '$' => '4',
                '%' => '5',
                '^' => '6',
                '&' => '7',
                '*' => '8',
                '(' => '9',
                ')' => '0',
                ':' => ';',
                '+' => '=',
                '<' => ',',
                '_' => '-',
                '>' => '.',
                '?' => '/',
                '~' => '`',
                '{' => '[',
                '|' => '\\',
                '}' => ']',
                '"' => '\'',
                _ => return None,
            };
            let (vk, sc, _) = char_key(base)?;
            Some((vk, sc, true))
        }
    }
}

/// What Windows Terminal writes into a pseudo-console for `k`: the key-down
/// record, then the key-up, in win32-input-mode.
pub fn win32_input(k: &Key) -> Result<Vec<u8>, String> {
    let mut state = 0u32;
    if k.ctrl {
        state |= LEFT_CTRL_PRESSED;
    }
    if k.alt {
        state |= LEFT_ALT_PRESSED;
    }
    if k.shift {
        state |= SHIFT_PRESSED;
    }
    let nav = |vk: u16, sc: u16| (vk, sc, 0u32, ENHANCED_KEY);
    let (vk, sc, uc, extra) = match k.code {
        Code::Enter => (0x0D, 0x1C, 0x0D, 0),
        Code::Escape => (0x1B, 0x01, 0x1B, 0),
        Code::Tab => (0x09, 0x0F, 0x09, 0),
        Code::Backspace => (0x08, 0x0E, 0x08, 0),
        Code::Left => nav(0x25, 0x4B),
        Code::Up => nav(0x26, 0x48),
        Code::Right => nav(0x27, 0x4D),
        Code::Down => nav(0x28, 0x50),
        Code::Home => nav(0x24, 0x47),
        Code::End => nav(0x23, 0x4F),
        Code::PageUp => nav(0x21, 0x49),
        Code::PageDown => nav(0x22, 0x51),
        Code::Insert => nav(0x2D, 0x52),
        Code::Delete => nav(0x2E, 0x53),
        Code::F(n @ 1..=10) => (0x70 + u16::from(n) - 1, 0x3B + u16::from(n) - 1, 0, 0),
        Code::F(11) => (0x7A, 0x57, 0, 0),
        Code::F(12) => (0x7B, 0x58, 0, 0),
        Code::F(n) => return Err(format!("there is no F{n} key to send")),
        Code::Char(c) => match char_key(c) {
            Some((vk, sc, shifted)) => {
                if shifted {
                    state |= SHIFT_PRESSED;
                }
                // ⚠️ The record carries the letter itself even with Ctrl.
                // A real Ctrl+F carries its control code (0x06), and for a
                // control code crossterm asks the *foreground window's*
                // keyboard layout which letter the key is: on a machine
                // whose foreground window types Cyrillic, Ctrl+F arrives as
                // Ctrl+'а'. With the letter in the record crossterm takes it
                // as is: the Ctrl+F a US layout gives, whatever is in front.
                (vk, sc, c as u32, 0)
            }
            None => {
                if c as u32 > 0xFFFF {
                    return Err(format!(
                        "'{c}' is outside the Basic Multilingual Plane; a console record \
                         carries one UTF-16 unit"
                    ));
                }
                (0, 0, c as u32, 0)
            }
        },
    };
    let state = state | extra;
    let mut out = format!("\x1b[{vk};{sc};{uc};1;{state};1_").into_bytes();
    out.extend(format!("\x1b[{vk};{sc};{uc};0;{state};1_").into_bytes());
    Ok(out)
}

/// The bytes for `k` on this platform's pseudo-terminal.
pub fn key_bytes(k: &Key) -> Result<Vec<u8>, String> {
    if cfg!(windows) {
        win32_input(k)
    } else {
        vt_bytes(k)
    }
}

/// A left click at a 0-based cell: SGR press then release (1-based on the
/// wire).
pub fn sgr_click(col: u16, row: u16) -> Vec<u8> {
    let (c, r) = (u32::from(col) + 1, u32::from(row) + 1);
    format!("\x1b[<0;{c};{r}M\x1b[<0;{c};{r}m").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(name: &str) -> Key {
        parse_key(name).unwrap()
    }

    fn vt(name: &str) -> Vec<u8> {
        vt_bytes(&k(name)).unwrap()
    }

    #[test]
    fn names_parse_as_the_suite_spells_them() {
        assert_eq!(k("enter").code, Code::Enter);
        assert_eq!(k("Escape").code, Code::Escape);
        assert_eq!(k("f2").code, Code::F(2));
        let ca = k("ctrl+a");
        assert_eq!((ca.code, ca.ctrl, ca.shift), (Code::Char('a'), true, false));
        let st = k("shift+tab");
        assert_eq!((st.code, st.shift), (Code::Tab, true));
        assert_eq!(k("shift+a").code, Code::Char('A'));
        assert!(!k("shift+a").shift, "shift+a is the character A");
        assert_eq!(k("ctrl-f").code, Code::Char('f'));
        assert_eq!(k("space").code, Code::Char(' '));
        assert_eq!(k("+").code, Code::Char('+'));
        assert_eq!(k("ctrl++").code, Code::Char('+'));
        for bad in ["", "f13", "hyper+a", "foo", "ctrl+"] {
            assert!(parse_key(bad).is_err(), "{bad:?} parsed");
        }
    }

    #[test]
    fn typing_is_one_key_per_character() {
        let keys = typed("Ab1 \n");
        let codes: Vec<Code> = keys.iter().map(|k| k.code).collect();
        assert_eq!(
            codes,
            [
                Code::Char('A'),
                Code::Char('b'),
                Code::Char('1'),
                Code::Char(' '),
                Code::Enter
            ]
        );
    }

    /// The bytes crossterm's unix parser reads back as the same key.
    #[test]
    fn unix_bytes_are_what_an_xterm_sends() {
        assert_eq!(vt("enter"), b"\r");
        assert_eq!(vt("escape"), b"\x1b");
        assert_eq!(vt("tab"), b"\t");
        assert_eq!(vt("shift+tab"), b"\x1b[Z");
        assert_eq!(vt("backspace"), b"\x7f");
        assert_eq!(vt("delete"), b"\x1b[3~");
        assert_eq!(vt("left"), b"\x1b[D");
        assert_eq!(vt("right"), b"\x1b[C");
        assert_eq!(vt("up"), b"\x1b[A");
        assert_eq!(vt("down"), b"\x1b[B");
        assert_eq!(vt("home"), b"\x1b[H");
        assert_eq!(vt("end"), b"\x1b[F");
        assert_eq!(vt("shift+right"), b"\x1b[1;2C");
        assert_eq!(vt("ctrl+left"), b"\x1b[1;5D");
        assert_eq!(vt("f2"), b"\x1bOQ");
        assert_eq!(vt("shift+f2"), b"\x1b[1;2Q");
        assert_eq!(vt("f12"), b"\x1b[24~");
        assert_eq!(vt("ctrl+a"), [0x01]);
        assert_eq!(vt("ctrl+h"), [0x08], "Ctrl+H, not Backspace (0x7F)");
        assert_eq!(vt("ctrl+c"), [0x03]);
        assert_eq!(vt("ctrl+v"), [0x16]);
        assert_eq!(vt("alt+f"), b"\x1bf");
        assert_eq!(vt("A"), b"A");
        assert_eq!(vt("é"), "é".as_bytes());
        assert!(vt_bytes(&k("ctrl+shift+a")).is_err(), "no byte for it");
    }

    /// A key-down record and its key-up, as Windows Terminal writes them.
    #[test]
    fn windows_keys_are_win32_input_mode_records() {
        let w = |name: &str| String::from_utf8(win32_input(&k(name)).unwrap()).unwrap();
        assert_eq!(w("enter"), "\x1b[13;28;13;1;0;1_\x1b[13;28;13;0;0;1_");
        assert_eq!(w("escape"), "\x1b[27;1;27;1;0;1_\x1b[27;1;27;0;0;1_");
        assert_eq!(w("backspace"), "\x1b[8;14;8;1;0;1_\x1b[8;14;8;0;0;1_");
        // Ctrl+H is the H key with Ctrl, never Backspace.
        assert_eq!(w("ctrl+h"), "\x1b[72;35;104;1;8;1_\x1b[72;35;104;0;8;1_");
        // The letter, not its control code: see `win32_input`.
        assert_eq!(w("ctrl+a"), "\x1b[65;30;97;1;8;1_\x1b[65;30;97;0;8;1_");
        assert_eq!(w("alt+f"), "\x1b[70;33;102;1;2;1_\x1b[70;33;102;0;2;1_");
        // A capital letter is the letter key with Shift.
        assert_eq!(w("A"), "\x1b[65;30;65;1;16;1_\x1b[65;30;65;0;16;1_");
        assert_eq!(w("1"), "\x1b[49;2;49;1;0;1_\x1b[49;2;49;0;0;1_");
        assert_eq!(
            win32_input(&typed(" ")[0]).unwrap(),
            win32_input(&k("space")).unwrap()
        );
        assert_eq!(w(":"), "\x1b[186;39;58;1;16;1_\x1b[186;39;58;0;16;1_");
        // Navigation keys are enhanced (0x100).
        assert_eq!(w("left"), "\x1b[37;75;0;1;256;1_\x1b[37;75;0;0;256;1_");
        assert_eq!(
            w("shift+right"),
            "\x1b[39;77;0;1;272;1_\x1b[39;77;0;0;272;1_"
        );
        assert_eq!(w("shift+tab"), "\x1b[9;15;9;1;16;1_\x1b[9;15;9;0;16;1_");
        assert_eq!(w("f2"), "\x1b[113;60;0;1;0;1_\x1b[113;60;0;0;0;1_");
        assert_eq!(w("f12"), "\x1b[123;88;0;1;0;1_\x1b[123;88;0;0;0;1_");
        // No US key types it: a bare Unicode record.
        assert_eq!(w("é"), "\x1b[0;0;233;1;0;1_\x1b[0;0;233;0;0;1_");
    }

    #[test]
    fn a_click_is_an_sgr_press_and_release_one_based() {
        assert_eq!(sgr_click(0, 0), b"\x1b[<0;1;1M\x1b[<0;1;1m");
        assert_eq!(sgr_click(9, 39), b"\x1b[<0;10;40M\x1b[<0;10;40m");
    }
}
