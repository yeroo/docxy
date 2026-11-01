//! Task custom fields of current Project `.mpp` files (#292): their values
//! from the task VarMeta and their definitions from `TBkndTask/Props`.
//!
//! Evidence is the snapshot corpus (`corpus/mpp/snapshots`): 22-hyperlink has
//! no custom field, 23-custom-fields adds task UID 33 with Text1, Flag1,
//! Number1 and Date1 set, and 45-baseline1 saves an interim plan into Start1
//! and Finish1 on every task. Each paired `.xml` is Project 2024's MSPDI of
//! the same plan.
//!
//! **Values.** A task value is a VarMeta entry (tag `0x0b40`) whose key is
//! the low word of the field's MSPDI FieldID (`0x0B40_0000 | key`): Text1
//! `0x0033` (UTF-16 ending in a NUL), Start1 `0x0034`, Finish1 `0x0035` and
//! Date1 `0x0109` (the four-byte task timestamp), Number1 `0x0057` (an `f64`).
//! Only the keys in [`FIELDS`] are read: UID 33 also carries keys
//! `0x00a4`..`0x00aa` holding 0.0 that Project's XML does not list. A value
//! that does not decode (an NA date, an empty text, a non-finite number) is
//! left out, never an error.
//!
//! **Flag1 is not read.** It is no VarMeta entry; the snapshots set two
//! Fixed2Meta bits on UID 33 only (byte 0x1f bit 7, byte 0x3d bit 7), but
//! UID 33 also carries field 188744339 (`0x0B40_0293`, no definition) and
//! nothing in the corpus tells which bit is which. Its definition is read.
//!
//! **Definitions.** `TBkndTask/Props` is a Props stream ([`crate::props`]
//! layout) whose key `0x0440_0001` holds, after 36 bytes not yet understood,
//! a `u32` record count, a `u32` byte length (`count * 88`) and the 88-byte
//! records, ascending by FieldID: FieldID `u32` at +0, a type `u32` at +4,
//! SecondaryPID `u32` at +8 and the Guid (16 bytes, Windows layout) at +20.
//! SecondaryGuid is not stored: in all six definitions of the corpus it is
//! the Guid's first 28 characters followed by the SecondaryPID in hex.
//! FieldName is not stored either, so a record whose FieldID is not in
//! [`FIELDS`] is skipped. A stream that does not have this shape gives no
//! definitions, never an error. Aliases and lookup tables have no corpus
//! case and are not read.
use projcore::model::{ExtendedAttributeValue, XmlElement};

/// How a field's VarMeta value is stored; `None` in [`FIELDS`] is a field
/// whose value is not read (Flag1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Text,
    Number,
    Date,
}

/// The custom fields the corpus proves: the FieldID's low word (the VarMeta
/// key), Project's field name and the value's storage.
const FIELDS: [(u16, &str, Option<Kind>); 6] = [
    (0x0033, "Text1", Some(Kind::Text)),
    (0x0034, "Start1", Some(Kind::Date)),
    (0x0035, "Finish1", Some(Kind::Date)),
    (0x0048, "Flag1", None),
    (0x0057, "Number1", Some(Kind::Number)),
    (0x0109, "Date1", Some(Kind::Date)),
];

/// The high word of a task custom field's MSPDI FieldID.
const TASK_FIELD: u32 = 0x0B40_0000;

/// The `TBkndTask/Props` key holding the definitions.
const DEFINITIONS_KEY: u32 = 0x0440_0001;
const DEFINITION_LEN: usize = 88;
/// Bytes of the definitions value before the record count.
const DEFINITIONS_HEAD: usize = 36;

/// The MSPDI value of the task VarMeta entry `key`, when `key` is a custom
/// field read here and the value decodes.
pub(crate) fn value(key: u16, value: &[u8]) -> Option<ExtendedAttributeValue> {
    let (_, _, kind) = FIELDS.iter().find(|f| f.0 == key)?;
    let text = match (*kind)? {
        Kind::Text => crate::utf16::terminated(value, 2)
            .ok()
            .filter(|s| !s.is_empty() && !s.chars().any(char::is_control))?,
        Kind::Number => {
            let n = f64::from_le_bytes(value.try_into().ok()?);
            // MSPDI prints a whole number without a fraction (`42`); Rust's
            // shortest round-trip form does the same.
            n.is_finite().then(|| format!("{n}"))?
        }
        Kind::Date => {
            if value.len() != 4 {
                return None;
            }
            let at = crate::mpp::decode_checked_timestamp(value, 0).ok()??;
            mspdi_date(&at)?
        }
    };
    Some(ExtendedAttributeValue {
        field_id: (TASK_FIELD | u32::from(key)).to_string(),
        value: Some(text),
        ..ExtendedAttributeValue::default()
    })
}

/// `YYYY-MM-DD HH:MM` (the task timestamp) as MSPDI's `YYYY-MM-DDTHH:MM:00`.
fn mspdi_date(at: &str) -> Option<String> {
    let (date, time) = at.split_once(' ')?;
    (date.len() == 10 && time.len() == 5).then(|| format!("{date}T{time}:00"))
}

/// The definitions of a `TBkndTask/Props` stream, in stream (FieldID) order.
pub(crate) fn definitions(props: &[u8]) -> Vec<XmlElement> {
    let Ok(entries) = crate::props::entries(props) else {
        return Vec::new();
    };
    let Some(block) = entries.get(&DEFINITIONS_KEY) else {
        return Vec::new();
    };
    let u32_at = |o: usize| {
        block
            .get(o..o + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let (Some(count), Some(len)) = (u32_at(DEFINITIONS_HEAD), u32_at(DEFINITIONS_HEAD + 4)) else {
        return Vec::new();
    };
    let records = DEFINITIONS_HEAD + 8;
    let (count, len) = (count as usize, len as usize);
    if count.checked_mul(DEFINITION_LEN) != Some(len)
        || records.checked_add(len) != Some(block.len())
    {
        return Vec::new();
    }
    block[records..]
        .as_chunks::<DEFINITION_LEN>()
        .0
        .iter()
        .filter_map(definition)
        .collect()
}

fn definition(rec: &[u8; DEFINITION_LEN]) -> Option<XmlElement> {
    let u32_at = |o: usize| u32::from_le_bytes([rec[o], rec[o + 1], rec[o + 2], rec[o + 3]]);
    let field_id = u32_at(0);
    if field_id & 0xFFFF_0000 != TASK_FIELD {
        return None;
    }
    let (_, name, _) = FIELDS
        .iter()
        .find(|f| u32::from(f.0) == field_id & 0xFFFF)?;
    let secondary_pid = u32_at(8);
    let guid = crate::taskdecode::guid_text(&rec[20..36]);
    let secondary_guid = format!("{}{secondary_pid:08X}", &guid[..28]);
    let leaf = |name: &str, text: String| XmlElement {
        name: name.into(),
        text,
        children: Vec::new(),
    };
    Some(XmlElement {
        name: "ExtendedAttribute".into(),
        text: String::new(),
        children: vec![
            leaf("FieldID", field_id.to_string()),
            leaf("FieldName", (*name).into()),
            leaf("Guid", guid),
            leaf("SecondaryPID", secondary_pid.to_string()),
            leaf("SecondaryGuid", secondary_guid),
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    #[test]
    fn values_decode_as_mspdi_prints_them() {
        let text = value(0x0033, &utf16("corpus-text\0")).unwrap();
        assert_eq!(text.field_id, "188743731");
        assert_eq!(text.value.as_deref(), Some("corpus-text"));
        let number = value(0x0057, &42f64.to_le_bytes()).unwrap();
        assert_eq!(number.value.as_deref(), Some("42"));
        assert_eq!(
            value(0x0057, &1.5f64.to_le_bytes())
                .unwrap()
                .value
                .as_deref(),
            Some("1.5")
        );
        // 23-custom-fields UID 33 Date1: 2025-02-05T08:00:00.
        let date = value(0x0109, &[0xc0, 0x12, 0xa4, 0x3a]).unwrap();
        assert_eq!(date.field_id, "188743945");
        assert_eq!(date.value.as_deref(), Some("2025-02-05T08:00:00"));
    }

    #[test]
    fn unknown_keys_and_malformed_values_are_left_out() {
        // A key outside the table (UID 33's 0x00a4 holds 0.0), Flag1, which
        // is not read, and malformed values of each kind.
        assert_eq!(value(0x00a4, &0f64.to_le_bytes()), None);
        assert_eq!(value(0x0048, &[1, 0]), None);
        assert_eq!(value(0x0033, &utf16("no nul")), None);
        assert_eq!(value(0x0033, &utf16("\0")), None);
        assert_eq!(value(0x0033, &[0x41]), None);
        assert_eq!(value(0x0057, &[0; 4]), None);
        assert_eq!(value(0x0057, &f64::NAN.to_le_bytes()), None);
        assert_eq!(value(0x0109, &[0xff, 0xff, 0xff, 0xff]), None);
        assert_eq!(value(0x0109, &[0, 0, 0]), None);
    }

    /// A `TBkndTask/Props` stream holding `records` under the definitions
    /// key, with `count` records and `declared` bytes counted.
    fn props(records: &[Vec<u8>], count: u32, declared: u32) -> Vec<u8> {
        let mut value = vec![0u8; DEFINITIONS_HEAD];
        value.extend(count.to_le_bytes());
        value.extend(declared.to_le_bytes());
        for r in records {
            value.extend(r);
        }
        let mut entry = (value.len() as u32).to_le_bytes().to_vec();
        entry.extend(DEFINITIONS_KEY.to_le_bytes());
        entry.extend(0u32.to_le_bytes());
        entry.extend(&value);
        if value.len() % 2 == 1 {
            entry.push(0);
        }
        let total = 16 + entry.len();
        let mut b = ((total - 4) as u32).to_le_bytes().to_vec();
        b.extend(((total - 4) as u32).to_le_bytes());
        b.extend(0x10000u32.to_le_bytes());
        b.extend(1u32.to_le_bytes());
        b.extend(entry);
        b
    }

    /// 23-custom-fields' Text1 record.
    fn text1() -> Vec<u8> {
        let mut r = vec![0u8; DEFINITION_LEN];
        r[..4].copy_from_slice(&0x0B40_0033u32.to_le_bytes());
        r[4..8].copy_from_slice(&8u32.to_le_bytes());
        r[8..12].copy_from_slice(&0x0F40_4064u32.to_le_bytes());
        r[12..16].copy_from_slice(&7u32.to_le_bytes());
        r[20..36].copy_from_slice(&[
            0xb7, 0x39, 0, 0, 0xbe, 0x8b, 0xeb, 0x4c, 0x82, 0xc4, 0xfa, 0x8c, 0x0b, 0x40, 0, 0x33,
        ]);
        r
    }

    #[test]
    fn definitions_decode_as_project_writes_them() {
        let defs = definitions(&props(&[text1()], 1, 88));
        let leaves: Vec<_> = defs[0]
            .children
            .iter()
            .map(|c| (c.name.as_str(), c.text.as_str()))
            .collect();
        assert_eq!(
            leaves,
            [
                ("FieldID", "188743731"),
                ("FieldName", "Text1"),
                ("Guid", "000039B7-8BBE-4CEB-82C4-FA8C0B400033"),
                ("SecondaryPID", "255869028"),
                ("SecondaryGuid", "000039B7-8BBE-4CEB-82C4-FA8C0F404064"),
            ]
        );
    }

    #[test]
    fn a_malformed_definitions_block_gives_none() {
        // A count that overruns the stream, a length that is not the count's,
        // a record of a field with no known name, and no Props at all.
        assert!(definitions(&props(&[text1()], 2, 176)).is_empty());
        assert!(definitions(&props(&[text1()], 1, 176)).is_empty());
        let mut short = props(&[text1()], 1, 88);
        let n = short.len();
        short.truncate(n - 2);
        assert!(definitions(&short).is_empty());
        let mut unknown = text1();
        unknown[..4].copy_from_slice(&0x0B40_0293u32.to_le_bytes());
        assert!(definitions(&props(&[unknown], 1, 88)).is_empty());
        let mut resource = text1();
        resource[..4].copy_from_slice(&0x0C40_0033u32.to_le_bytes());
        assert!(definitions(&props(&[resource], 1, 88)).is_empty());
        assert!(definitions(&[]).is_empty());
    }
}
