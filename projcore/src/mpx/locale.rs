//! The words, duration units and link types of the locales MPXJ's MPX
//! writer and reader know (#1805), and the choice of one for a file.
//!
//! Every word is copied from MPXJ 16.8.0's `org.mpxj.mpx.LocaleData_<l>`
//! bundle (keys `YES`, `NO`, `CONSTRAINT_TYPES`, `PRIORITY_TYPES`,
//! `ACCRUE_TYPES`, `RELATION_TYPES`, `TIME_UNITS_ARRAY`), the bundle a
//! localised MPXJ or Project file is written with. Chinese (`ZH`) is not
//! here: its code page (936) has no decoder in `opccore::codepage`.
//!
//! The units are listed by meaning, never by the bundle's index: English's
//! bundle orders months, `%` and years differently from the others. `%`
//! and its elapsed form are lag-only and read by [`super::Reader::link_entry`]
//! before any unit table. French's elapsed year is MPXJ's placeholder `???`,
//! left out.

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::model::{AccrueAt, ConstraintType, LagUnit};

/// A duration unit: what a suffix means.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Unit {
    pub unit: LagUnit,
    pub elapsed: bool,
    /// A year, which has no MSPDI format and is kept in days.
    pub year: bool,
}

const fn u(unit: LagUnit, elapsed: bool, year: bool) -> Unit {
    Unit {
        unit,
        elapsed,
        year,
    }
}

const MIN: Unit = u(LagUnit::Minute, false, false);
const HOUR: Unit = u(LagUnit::Hour, false, false);
const DAY: Unit = u(LagUnit::Day, false, false);
const WEEK: Unit = u(LagUnit::Week, false, false);
const MONTH: Unit = u(LagUnit::Month, false, false);
const YEAR: Unit = u(LagUnit::Day, false, true);
const EMIN: Unit = u(LagUnit::Minute, true, false);
const EHOUR: Unit = u(LagUnit::Hour, true, false);
const EDAY: Unit = u(LagUnit::Day, true, false);
const EWEEK: Unit = u(LagUnit::Week, true, false);
const EMONTH: Unit = u(LagUnit::Month, true, false);
const EYEAR: Unit = u(LagUnit::Day, true, true);

/// One locale's words. Lists are in the model's order: constraints in
/// [`ConstraintType`] code order (ASAP, ALAP, MSO, MFO, SNET, SNLT, FNET,
/// FNLT), priorities 100 to 1000, Accrue At Start/End/Prorated, link types
/// FF, FS, SF, SS (MPXJ's `RELATION_TYPES` order).
#[derive(Debug)]
pub(super) struct Locale {
    yes: &'static [&'static str],
    no: &'static [&'static str],
    constraints: [&'static str; 8],
    priorities: [&'static str; 10],
    accrue: [&'static str; 3],
    relations: [&'static str; 4],
    /// Suffix and meaning, any order: [`Locale::unit`] takes the longest.
    units: &'static [(&'static str, Unit)],
}

/// English, also the fallback of every other locale. The units add
/// Project's own `mo`/`emo` to MPXJ's.
pub(super) static EN: Locale = Locale {
    yes: &["Yes"],
    no: &["No"],
    constraints: [
        "As Soon As Possible",
        "As Late As Possible",
        "Must Start On",
        "Must Finish On",
        "Start No Earlier Than",
        "Start No Later Than",
        "Finish No Earlier Than",
        "Finish No Later Than",
    ],
    priorities: [
        "Lowest",
        "Very Low",
        "Lower",
        "Low",
        "Medium",
        "High",
        "Higher",
        "Very High",
        "Highest",
        "Do Not Level",
    ],
    accrue: ["Start", "End", "Prorated"],
    relations: ["FF", "FS", "SF", "SS"],
    units: &[
        ("m", MIN),
        ("mins", MIN),
        ("h", HOUR),
        ("hours", HOUR),
        ("d", DAY),
        ("days", DAY),
        ("w", WEEK),
        ("wk", WEEK),
        ("weeks", WEEK),
        ("mon", MONTH),
        ("months", MONTH),
        ("mo", MONTH),
        ("y", YEAR),
        ("years", YEAR),
        ("em", EMIN),
        ("eh", EHOUR),
        ("ed", EDAY),
        ("ew", EWEEK),
        ("emon", EMONTH),
        ("emo", EMONTH),
        ("ey", EYEAR),
    ],
};

static DE: Locale = Locale {
    yes: &["Ja"],
    no: &["Nein"],
    constraints: [
        "So früh wie möglich",
        "So spät wie möglich",
        "Muss anfangen am",
        "Muss enden am",
        "Anfang nicht früher als",
        "Anfang nicht später als",
        "Ende nicht früher als",
        "Ende nicht später als",
    ],
    priorities: [
        "Am niedrigsten",
        "Sehr niedrig",
        "Niedriger",
        "Niedrig",
        "Mittel",
        "Hoch",
        "Höher",
        "Sehr hoch",
        "Am höchsten",
        "Nicht abgleichen",
    ],
    accrue: ["Anfang", "Ende", "Anteilig"],
    relations: ["EE", "EA", "AE", "AA"],
    units: &[
        ("m", MIN),
        ("h", HOUR),
        ("t", DAY),
        ("w", WEEK),
        ("mon", MONTH),
        ("y", YEAR),
        ("fm", EMIN),
        ("fh", EHOUR),
        ("ft", EDAY),
        ("fw", EWEEK),
        ("fmon", EMONTH),
        ("fy", EYEAR),
    ],
};

static FR: Locale = Locale {
    yes: &["Oui"],
    no: &["Non"],
    constraints: [
        "Dès Que Possible",
        "Le Plus Tard Possible",
        "Doit Commencer Le",
        "Doit Finir Le",
        "Début Au Plus Tôt Le",
        "Début Au Plus Tard Le",
        "Fin Au Plus Tôt Le",
        "Fin Au Plus Tard Le",
    ],
    priorities: [
        "Le Plus Bas",
        "Très Bas",
        "Plus Bas",
        "Bas",
        "Moyen",
        "Elevé",
        "Plus Elevé",
        "Très Elevé",
        "Le Plus Elevé",
        "Ne Pas Niveler",
    ],
    accrue: ["Début", "Fin", "Proportion"],
    relations: ["FF", "FD", "DF", "DD"],
    units: &[
        ("m", MIN),
        ("h", HOUR),
        ("j", DAY),
        ("s", WEEK),
        ("ms", MONTH),
        ("a", YEAR),
        ("me", EMIN),
        ("he", EHOUR),
        ("je", EDAY),
        ("se", EWEEK),
        ("mse", EMONTH),
    ],
};

static IT: Locale = Locale {
    yes: &["Si"],
    no: &["No"],
    constraints: [
        "Appena Possibile",
        "Piu' Tardi Possibile",
        "Deve Cominciare il",
        "Deve Finire il",
        "Non iniziare prima di",
        "Non iniziare piu' tardi di",
        "Finire non prima",
        "Finire entro",
    ],
    priorities: [
        "Il minimo",
        "Molto Basso",
        "Piu' Basso",
        "Basso",
        "Medio",
        "Alto",
        "Piu' Alto",
        "Molto Alto",
        "Il maggiore",
        "Non livellare",
    ],
    accrue: ["Inizio", "Fine", "Proporzione"],
    relations: ["FF", "FS", "SF", "SS"],
    // `m` is a month here and `r` a minute.
    units: &[
        ("r", MIN),
        ("o", HOUR),
        ("g", DAY),
        ("s", WEEK),
        ("m", MONTH),
        ("a", YEAR),
        ("tr", EMIN),
        ("to", EHOUR),
        ("tg", EDAY),
        ("ts", EWEEK),
        ("tm", EMONTH),
        ("ta", EYEAR),
    ],
};

static PT: Locale = Locale {
    yes: &["Sim"],
    no: &["Não"],
    constraints: [
        "O Mais Breve Possível",
        "O Mais Tarde Possível",
        "Deve Iniciar Em",
        "Deve Terminar Em",
        "Não Iniciar Antes De",
        "Não Iniciar Depois De",
        "Não Terminar Antes De",
        "Não Terminar Depois De",
    ],
    // As the bundle has them: two are English.
    priorities: [
        "Muito Baixa",
        "Muito Baixo",
        "Lower",
        "Baixo",
        "Média",
        "Alta",
        "Higher",
        "Muito Alta",
        "Altíssimo",
        "Não Nivelar",
    ],
    // The bundle's Prorated is empty: never matched.
    accrue: ["Inicio", "Fim", ""],
    relations: ["TT", "TI", "IT", "II"],
    units: &[
        ("m", MIN),
        ("h", HOUR),
        ("d", DAY),
        ("s", WEEK),
        ("mes", MONTH),
        ("a", YEAR),
        ("em", EMIN),
        ("eh", EHOUR),
        ("ed", EDAY),
        ("es", EWEEK),
        ("emes", EMONTH),
        ("ea", EYEAR),
    ],
};

/// Swedish: only Yes/No and the hour (`t`) differ from English.
static SV: Locale = Locale {
    yes: &["Ja"],
    no: &["Nej"],
    constraints: EN.constraints,
    priorities: EN.priorities,
    accrue: EN.accrue,
    relations: EN.relations,
    units: &[
        ("m", MIN),
        ("t", HOUR),
        ("d", DAY),
        ("w", WEEK),
        ("mon", MONTH),
        ("y", YEAR),
        ("em", EMIN),
        ("eh", EHOUR),
        ("ed", EDAY),
        ("ew", EWEEK),
        ("emon", EMONTH),
        ("ey", EYEAR),
    ],
};

/// Spanish. The bundle's `Sí` and `Mínima` carry a U+00AD after the `í`,
/// which every comparison drops ([`folded`]), so Project's plain spelling
/// matches too.
static ES: Locale = Locale {
    yes: &["Sí\u{AD}"],
    no: &["No"],
    constraints: [
        "Lo antes posible",
        "Lo más tarde posible",
        "Debe comenzar el",
        "Debe finalizar el",
        "No comenzar antes del",
        "No comenzar después del",
        "No finalizar antes del",
        "No finalizar después del",
    ],
    priorities: [
        "Mí\u{AD}nima",
        "Muy Baja",
        "Más Baja",
        "Baja",
        "Media",
        "Alta",
        "Más Alta",
        "Muy Alta",
        "Máxima",
        "No Redistribuir",
    ],
    accrue: ["Comienzo", "Fin", "Prorrateo"],
    relations: ["FF", "FC", "CF", "CC"],
    units: &[
        ("m", MIN),
        ("h", HOUR),
        ("d", DAY),
        ("s", WEEK),
        ("ms", MONTH),
        ("a", YEAR),
        ("em", EMIN),
        ("eh", EHOUR),
        ("ed", EDAY),
        ("es", EWEEK),
        ("ems", EMONTH),
        ("ea", EYEAR),
    ],
};

/// Russian, written in Windows-1251 (header code page `RU`).
pub(super) static RU: Locale = Locale {
    yes: &["Да"],
    no: &["Нет"],
    constraints: [
        "Как можно раньше",
        "Как можно позже",
        "Должен начаться",
        "Должен закончиться",
        "Начаться не раньше",
        "Начаться не позже",
        "Закончиться не раньше",
        "Закончиться не позже",
    ],
    priorities: [
        "Самый низкий",
        "Очень низкий",
        "Низкий",
        "Ниже среднего",
        "Средний",
        "Выше среднего",
        "Высокий",
        "Очень высокий",
        "Наивысший",
        "Без приоритета",
    ],
    accrue: ["Начало", "Конец", "Повседневный"],
    relations: ["КК", "КН", "НК", "НН"],
    units: &[
        ("минута", MIN),
        ("час", HOUR),
        ("день", DAY),
        ("неделя", WEEK),
        ("месяц", MONTH),
        ("год", YEAR),
        ("каждую минуту", EMIN),
        ("каждый час", EHOUR),
        ("каждый день", EDAY),
        ("каждую неделю", EWEEK),
        ("каждый месяц", EMONTH),
        ("каждый год", EYEAR),
    ],
};

/// Every locale, English first: the order ties are broken in.
pub(super) static LOCALES: [&Locale; 8] = [&EN, &DE, &FR, &IT, &PT, &SV, &ES, &RU];

/// `text` as words are compared: trimmed, without U+00AD, lower case.
fn folded(text: &str) -> impl Iterator<Item = char> + '_ {
    text.trim()
        .chars()
        .filter(|&c| c != '\u{AD}')
        .flat_map(char::to_lowercase)
}

fn position(list: &[&str], text: &str) -> Option<usize> {
    list.iter()
        .position(|w| !w.is_empty() && folded(w).eq(folded(text)))
}

impl Locale {
    /// A word this locale's Yes/No names.
    fn own_yes_no(&self, text: &str) -> Option<bool> {
        if position(self.yes, text).is_some() {
            Some(true)
        } else if position(self.no, text).is_some() {
            Some(false)
        } else {
            None
        }
    }

    /// Yes or No, in this locale's words, else English's, else `1`/`0` or
    /// `True`/`False` (which never count towards a locale's score).
    pub fn yes_no(&self, text: &str) -> Option<bool> {
        self.own_yes_no(text)
            .or_else(|| EN.own_yes_no(text))
            .or_else(|| match text.trim().to_ascii_lowercase().as_str() {
                "1" | "true" => Some(true),
                "0" | "false" => Some(false),
                _ => None,
            })
    }

    /// A constraint type by its name here or in English, or its code.
    pub fn constraint_type(&self, text: &str) -> Option<ConstraintType> {
        let index = position(&self.constraints, text).or_else(|| position(&EN.constraints, text));
        match index {
            Some(i) => ConstraintType::from_code(i as i64),
            None => text.trim().parse().ok().and_then(ConstraintType::from_code),
        }
    }

    /// A priority by its name here or in English (100 to 1000, as MPXJ maps
    /// them: Medium is 500), or a number from 0 to 1000.
    pub fn priority(&self, text: &str) -> Option<i32> {
        let index = position(&self.priorities, text).or_else(|| position(&EN.priorities, text));
        match index {
            Some(i) => Some((i as i32 + 1) * 100),
            None => text.trim().parse().ok().filter(|n| (0..=1000).contains(n)),
        }
    }

    /// Accrue At by its name here or in English.
    pub fn accrue_at(&self, text: &str) -> Option<AccrueAt> {
        let index = position(&self.accrue, text).or_else(|| position(&EN.accrue, text))?;
        Some([AccrueAt::Start, AccrueAt::End, AccrueAt::Prorated][index])
    }

    /// The English link type (`FS`) of this locale's (`EA`), any case.
    fn relation(&self, letters: &str) -> Option<&'static str> {
        position(&self.relations, letters).map(|i| EN.relations[i])
    }

    /// A Predecessors entry with its link type in English, so the cell
    /// grammar reads it: the letters straight after the leading task ID,
    /// when they are this locale's name of a type. Anything else (an
    /// English type, a path) is left as it is.
    pub fn english_link(&self, entry: &str) -> String {
        let Some(start) = entry.find(|c: char| !c.is_ascii_digit() && !c.is_whitespace()) else {
            return entry.to_string();
        };
        let end = entry[start..]
            .find(|c: char| !c.is_alphabetic())
            .map_or(entry.len(), |i| start + i);
        if start == 0 || start == end {
            return entry.to_string();
        }
        match self.relation(&entry[start..end]) {
            Some(english) => format!("{}{english}{}", &entry[..start], &entry[end..]),
            None => entry.to_string(),
        }
    }

    /// The number before a unit suffix of this locale's, and the unit: the
    /// longest suffix that matches. `text` is lower case.
    fn own_unit<'a>(&self, text: &'a str) -> Option<(&'a str, Unit)> {
        self.units
            .iter()
            .filter_map(|&(suffix, unit)| {
                text.strip_suffix(suffix)
                    .map(|n| (suffix.chars().count(), n, unit))
            })
            .max_by_key(|(len, _, _)| *len)
            .map(|(_, n, unit)| (n.trim_end(), unit))
    }

    /// The unit a rate's `/unit` names exactly (`h`, German `t`, Russian
    /// `час`), this locale's, else English's.
    pub fn rate_unit(&self, text: &str) -> Option<Unit> {
        let exact = |l: &Locale| {
            l.units
                .iter()
                .find(|(suffix, _)| *suffix == text)
                .map(|&(_, u)| u)
        };
        exact(self).or_else(|| exact(&EN))
    }

    /// The number and unit of a duration (lower case, without its `?`): by
    /// this locale's suffixes when what is left is a number, else English's
    /// (German `2em` is no `2e` in minutes but English's elapsed minutes);
    /// `None` for a bare number.
    pub fn unit<'a>(&self, text: &'a str) -> Option<(&'a str, Unit)> {
        self.own_unit(text)
            .filter(|(number, _)| is_number(number))
            .or_else(|| EN.own_unit(text))
    }
}

/// The locales (a bit per index in [`LOCALES`]) whose Yes/No, constraint,
/// priority or Accrue At words hold each word, [`folded`]: built once, as
/// the choice looks up every field of a file.
fn word_locales() -> &'static HashMap<String, u8> {
    static WORDS: OnceLock<HashMap<String, u8>> = OnceLock::new();
    WORDS.get_or_init(|| {
        let mut words: HashMap<String, u8> = HashMap::new();
        for (i, l) in LOCALES.iter().enumerate() {
            let lists = [
                l.yes,
                l.no,
                &l.constraints[..],
                &l.priorities[..],
                &l.accrue[..],
            ];
            for w in lists.into_iter().flatten().filter(|w| !w.is_empty()) {
                *words.entry(folded(w).collect()).or_default() |= 1 << i;
            }
        }
        words
    })
}

/// What a field may say about the locale: a word (Yes/No, constraint
/// type, priority, Accrue At), a duration or work with its unit, or the
/// Predecessors list. Free text (names, WBS, Text, notes) says nothing.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    Word,
    Duration,
    Links,
}

/// The kind of a task row's field by its id (record 61), as MPXJ 16.8.0's
/// `MPXTaskField` types them: BOOLEAN, TASK_TYPE (Fixed), CONSTRAINT and
/// PRIORITY are words, DURATION and WORK durations, Predecessors the links
/// (Successors, 71, is not read).
fn task_kind(id: u32) -> Option<Kind> {
    match id {
        80..=84 | 91 | 95 | 110..=120 | 122 | 123 | 135 | 136 => Some(Kind::Word),
        20..=24 | 40..=43 | 45..=48 | 66 | 67 | 92..=94 => Some(Kind::Duration),
        70 => Some(Kind::Links),
        _ => None,
    }
}

/// The kind of a resource row's field by its id (record 41), by
/// `MPXResourceField`: Accrue At and the BOOLEAN fields are words, WORK and
/// DURATION durations.
fn resource_kind(id: u32) -> Option<Kind> {
    match id {
        45 | 46 | 51 => Some(Kind::Word),
        20..=25 => Some(Kind::Duration),
        _ => None,
    }
}

/// The kind of an assignment's field by its place (record 75 is fixed: work,
/// baseline, actual and overtime work, then the delay).
fn assignment_kind(place: usize) -> Option<Kind> {
    matches!(place, 3..=6 | 12).then_some(Kind::Duration)
}

/// Score a file's field `value` of `kind` for each locale: a word field one
/// for each locale whose Yes/No, constraint, priority or Accrue At word it
/// is, a duration one for each locale whose unit it has, when English does
/// not read it (`2t`).
fn score_field(scores: &mut [u32; LOCALES.len()], value: &str, kind: Kind) {
    if !value.contains(char::is_alphabetic) {
        return;
    }
    if kind == Kind::Duration {
        let low = lower(value);
        // What English reads as a number and a unit is no evidence.
        if matches!(EN.own_unit(&low), Some((number, _)) if is_number(number)) {
            return;
        }
        for (score, l) in scores.iter_mut().zip(LOCALES).skip(1) {
            if matches!(l.own_unit(&low), Some((number, _)) if is_number(number)) {
                *score += 1;
            }
        }
    } else if let Some(mask) = word_locales().get(&folded(value).collect::<String>()) {
        // A word field.
        for (i, score) in scores.iter_mut().enumerate() {
            *score += u32::from(mask & (1 << i) != 0);
        }
    }
}

/// Score one Predecessors entry: one for each locale whose name of its type
/// it is, when English's is not (`2EA+1t`), and its lag as a field.
fn score_link(scores: &mut [u32; LOCALES.len()], entry: &str) {
    if EN.english_link(entry) == entry {
        for (score, l) in scores.iter_mut().zip(LOCALES).skip(1) {
            *score += u32::from(l.english_link(entry) != entry);
        }
    }
    if let Some(sign) = entry.find(['+', '-']) {
        score_field(scores, &entry[sign + 1..], Kind::Duration);
    }
}

/// A field as [`score_field`] reads its unit: trimmed, lower case, without
/// the estimate's `?`.
fn lower(value: &str) -> String {
    let t = value.trim().to_lowercase();
    match t.strip_suffix('?') {
        Some(rest) => rest.trim_end().to_string(),
        None => t,
    }
}

/// Digits, with a decimal or thousands mark: the number before a unit.
fn is_number(text: &str) -> bool {
    text.chars().any(|c| c.is_ascii_digit())
        && text
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | ',' | ' ' | '\'' | '\u{A0}'))
}

/// The locale a file is written in, from its records (the header first).
/// `RU` in the header's code page field is Russian. Otherwise each locale
/// scores the word and duration fields of the task (70), resource (50) and
/// assignment (75) rows, by the ids of the last record 61 and 41
/// ([`task_kind`], [`resource_kind`], [`assignment_kind`]; never free text
/// such as a name, so a task named `Fine` says nothing), with
/// [`score_field`], and the entries of the task rows' Predecessors field
/// (split on `delimiter`, [`score_link`]). A locale other than English wins
/// only with a higher score than every locale before it in [`LOCALES`], so
/// English wins ties and a file with no evidence.
///
/// A known limit: a German file whose only evidence is days (`2t`) ties
/// with Swedish (`t` hours) and is read as German, the first.
pub(super) fn choose(records: &[Vec<String>], delimiter: char) -> &'static Locale {
    let code_page = records
        .first()
        .and_then(|header| header.get(3))
        .map(|f| f.trim().to_ascii_uppercase());
    if code_page.as_deref() == Some("RU") {
        return &RU;
    }
    let mut scores = [0u32; LOCALES.len()];
    let mut task_fields: Vec<u32> = Vec::new();
    let mut resource_fields: Vec<u32> = Vec::new();
    for record in records.iter().skip(1) {
        let Some(kind) = record.first().and_then(|k| k.trim().parse::<u32>().ok()) else {
            continue;
        };
        let field_kind = |i: usize| match kind {
            70 => task_fields.get(i).copied().and_then(task_kind),
            50 => resource_fields.get(i).copied().and_then(resource_kind),
            75 => assignment_kind(i + 1),
            _ => None,
        };
        match kind {
            61 => task_fields = super::ids(&record[1..]),
            41 => resource_fields = super::ids(&record[1..]),
            50 | 70 | 75 => {
                for (i, value) in record[1..].iter().enumerate() {
                    let value = value.trim();
                    match field_kind(i) {
                        _ if value.is_empty() => {}
                        Some(Kind::Links) => {
                            for entry in value.split(delimiter) {
                                score_link(&mut scores, entry.trim());
                            }
                        }
                        Some(kind) => score_field(&mut scores, value, kind),
                        None => {}
                    }
                }
            }
            _ => {}
        }
    }
    let mut best = 0;
    for (i, &score) in scores.iter().enumerate() {
        if score > scores[best] {
            best = i;
        }
    }
    LOCALES[best]
}
