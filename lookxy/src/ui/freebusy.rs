//! The free/busy availability grid — an overlay opened by `Ctrl-B` in the
//! event form. Read-only: shows each attendee's `availabilityView` as a strip
//! of busy/free glyphs plus a combined "everyone free" row. State + glyph
//! mapping live here; the draw/key handling is below (Task 4).

use mailcore::graph::model::ScheduleEntry;

pub struct FreeBusyView {
    pub day_label: String,
    pub interval_minutes: i64,
    pub slot_count: usize,
    pub entries: Vec<ScheduleEntry>,
    pub loading: bool,
}

/// One availability digit → its grid glyph. `'0'` free, `'1'` tentative,
/// `'2'`/`'3'`/`'4'` busy/OOF/elsewhere, anything else blank.
pub fn slot_glyph(c: char) -> char {
    match c {
        '0' => '·',
        '1' => '▓',
        '2' | '3' | '4' => '█',
        _ => ' ',
    }
}

/// The combined `free?`-row glyph for one slot across all entries: `'✓'` when
/// everyone is free (`'0'`, or past the end of a short string), `'█'` when
/// anyone is busy (`'2'`/`'3'`/`'4'`), else `'░'` (only tentatives).
pub fn combined_glyph(entries: &[ScheduleEntry], slot: usize) -> char {
    let mut any_busy = false;
    let mut any_tentative = false;
    for e in entries {
        match e.availability.chars().nth(slot) {
            Some('2') | Some('3') | Some('4') => any_busy = true,
            Some('1') => any_tentative = true,
            _ => {} // '0' or missing = free
        }
    }
    if any_busy {
        '█'
    } else if any_tentative {
        '░'
    } else {
        '✓'
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyph_mapping() {
        assert_eq!(slot_glyph('0'), '·');
        assert_eq!(slot_glyph('1'), '▓');
        assert_eq!(slot_glyph('2'), '█');
        let entries = vec![
            ScheduleEntry {
                email: "a".into(),
                availability: "02".into(),
            },
            ScheduleEntry {
                email: "b".into(),
                availability: "00".into(),
            },
        ];
        assert_eq!(combined_glyph(&entries, 0), '✓'); // both free
        assert_eq!(combined_glyph(&entries, 1), '█'); // a busy
        assert_eq!(combined_glyph(&entries, 5), '✓'); // past end = free
    }
}
