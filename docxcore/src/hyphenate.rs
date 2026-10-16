//! Where a word may take an optional hyphen (#1266), for Manual hyphenation.
//!
//! This is a small vowel/consonant syllable heuristic, not a dictionary: it
//! knows English-like spelling only (Latin letters; a word with no vowel it
//! knows gets no point), and it will propose some points a dictionary would
//! not and miss others. That is why Manual hyphenation asks the person: they
//! can move the hyphen in the box or answer No. A point always leaves at least
//! [`EDGE`] letters on each side of it.

/// Letters a point must leave on each side.
pub const EDGE: usize = 2;

/// The shortest word Manual hyphenation proposes.
pub const MIN_WORD: usize = 6;

fn is_vowel(c: char) -> bool {
    matches!(
        c,
        'a' | 'e'
            | 'i'
            | 'o'
            | 'u'
            | 'y'
            | 'à'
            | 'á'
            | 'â'
            | 'ä'
            | 'è'
            | 'é'
            | 'ê'
            | 'ë'
            | 'ì'
            | 'í'
            | 'î'
            | 'ï'
            | 'ò'
            | 'ó'
            | 'ô'
            | 'ö'
            | 'ù'
            | 'ú'
            | 'û'
            | 'ü'
    )
}

/// Two consonants that are one sound or a usual onset: they stay together.
fn inseparable(a: char, b: char) -> bool {
    matches!(
        (a, b),
        ('t' | 'c' | 's' | 'p' | 'w' | 'g', 'h')
            | ('c', 'k')
            | ('q', 'u')
            | ('b' | 'c' | 'f' | 'g' | 'p' | 's', 'l')
            | ('b' | 'c' | 'd' | 'f' | 'g' | 'p' | 't', 'r')
    )
}

/// Endings a point may sit just before.
const SUFFIXES: &[&str] = &[
    "tion", "sion", "ing", "ment", "ness", "able", "ible", "ful", "less", "ous", "ive", "ity",
    "ize", "ise", "ly",
];

/// The char indices in `word` where an optional hyphen may go: the hyphen
/// would sit before `word[i]`. Ascending, and empty for a word shorter than
/// [`MIN_WORD`] letters or with anything but letters in it.
pub fn hyphen_points(word: &str) -> Vec<usize> {
    let chars: Vec<char> = word.chars().collect();
    let n = chars.len();
    if n < MIN_WORD || !chars.iter().all(|c| c.is_alphabetic()) {
        return Vec::new();
    }
    let low: Vec<char> = word
        .chars()
        .map(|c| c.to_lowercase().next().unwrap_or(c))
        .collect();
    let vowel = |i: usize| is_vowel(low[i]);
    let mut points = Vec::new();
    for i in EDGE..=n - EDGE {
        // V-CV: before a lone consonant between vowels.
        let open = vowel(i - 1) && !vowel(i) && i + 1 < n && vowel(i + 1);
        // VC-CV: between two consonants, vowels around them, unless the
        // pair is one sound.
        let closed = i >= 2
            && i + 1 < n
            && vowel(i - 2)
            && !vowel(i - 1)
            && !vowel(i)
            && vowel(i + 1)
            && !inseparable(low[i - 1], low[i]);
        let suffix = SUFFIXES.iter().any(|s| {
            let s: Vec<char> = s.chars().collect();
            n - i == s.len() && low[i..] == s[..]
        });
        // A suffix point needs a vowel before it somewhere, so "string" is
        // not cut before "ing" with no syllable in front.
        let suffix = suffix && (0..i).any(vowel);
        if open || closed || suffix {
            points.push(i);
        }
    }
    points
}

/// The point to propose first: the one nearest the middle of the word, the
/// earlier of two equally near.
pub fn proposed_point(word: &str) -> Option<usize> {
    let n = word.chars().count();
    hyphen_points(word)
        .into_iter()
        .min_by_key(|&p| (2 * p).abs_diff(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_word_gets_syllable_points() {
        assert!(hyphen_points("computer").contains(&3), "com-puter");
        assert!(hyphen_points("hyphenation").contains(&7), "hyphena-tion");
        assert!(hyphen_points("beautiful").contains(&6), "beauti-ful");
        assert_eq!(hyphen_points("computer"), [3, 5], "com-pu-ter");
        assert!(hyphen_points("wanted").contains(&3), "wan-ted");
    }

    #[test]
    fn short_words_and_non_words_get_none() {
        for w in [
            "",
            "cat",
            "hello",
            "abcde",
            "co-operate",
            "abc123def",
            "e\u{2011}mail",
        ] {
            assert!(hyphen_points(w).is_empty(), "{w}");
        }
    }

    #[test]
    fn every_point_keeps_two_letters_on_each_side() {
        for w in [
            "computer",
            "hyphenation",
            "extraordinary",
            "wanted",
            "Information",
            "strength",
            "something",
            "beautiful",
        ] {
            let n = w.chars().count();
            for p in hyphen_points(w) {
                assert!(p >= EDGE && n - p >= EDGE, "{w}: {p}");
            }
        }
    }

    #[test]
    fn one_sound_pairs_stay_together() {
        // "ph" and "th" are not split by the VC-CV rule.
        assert!(!hyphen_points("hyphenation").contains(&3));
        assert!(!hyphen_points("mother").contains(&3), "mot-her");
    }

    #[test]
    fn a_suffix_needs_a_syllable_before_it() {
        assert!(hyphen_points("string").is_empty());
        assert!(hyphen_points("running").contains(&4), "runn-ing");
    }

    #[test]
    fn the_proposed_point_is_the_most_central_one() {
        let w = "extraordinary";
        let n = w.chars().count();
        let p = proposed_point(w).unwrap();
        for q in hyphen_points(w) {
            assert!((2 * p).abs_diff(n) <= (2 * q).abs_diff(n));
        }
        assert_eq!(proposed_point("cat"), None);
    }
}
