//! Minimal ZIP writer using the STORED (uncompressed) method.
//!
//! A STORED-only ZIP is a fully valid container that Word and every other reader
//! opens — so save needs no DEFLATE *compressor*. CRC-32 is computed correctly
//! (readers validate it). A real DEFLATE encoder for smaller files is a later
//! phase. Round-trips with [`crate::zip`].

const LOCAL_SIG: u32 = 0x04034b50;
const CENTRAL_SIG: u32 = 0x02014b50;
const EOCD_SIG: u32 = 0x06054b50;

/// CRC-32 (IEEE, reflected) as used by ZIP.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// Write a ZIP archive with every entry STORED, in the given order.
pub fn write_zip(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    let mut locals: Vec<u32> = Vec::with_capacity(entries.len());
    let mut crcs: Vec<u32> = Vec::with_capacity(entries.len());

    // General-purpose flag: bit 11 = filenames are UTF-8.
    const FLAGS: u16 = 0x0800;

    for (name, data) in entries {
        locals.push(out.len() as u32);
        let crc = crc32(data);
        crcs.push(crc);
        let nb = name.as_bytes();
        out.extend_from_slice(&LOCAL_SIG.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes()); // version needed
        out.extend_from_slice(&FLAGS.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // method = STORED
        out.extend_from_slice(&0u16.to_le_bytes()); // mod time
        out.extend_from_slice(&0x21u16.to_le_bytes()); // mod date (1980-01-01)
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes()); // comp size
        out.extend_from_slice(&(data.len() as u32).to_le_bytes()); // uncomp size
        out.extend_from_slice(&(nb.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra len
        out.extend_from_slice(nb);
        out.extend_from_slice(data);
    }

    let cd_offset = out.len() as u32;
    for (i, (name, data)) in entries.iter().enumerate() {
        let nb = name.as_bytes();
        out.extend_from_slice(&CENTRAL_SIG.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes()); // version made by
        out.extend_from_slice(&20u16.to_le_bytes()); // version needed
        out.extend_from_slice(&FLAGS.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // method
        out.extend_from_slice(&0u16.to_le_bytes()); // mod time
        out.extend_from_slice(&0x21u16.to_le_bytes()); // mod date
        out.extend_from_slice(&crcs[i].to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(nb.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra len
        out.extend_from_slice(&0u16.to_le_bytes()); // comment len
        out.extend_from_slice(&0u16.to_le_bytes()); // disk start
        out.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
        out.extend_from_slice(&0u32.to_le_bytes()); // external attrs
        out.extend_from_slice(&locals[i].to_le_bytes()); // local header offset
        out.extend_from_slice(nb);
    }
    let cd_size = out.len() as u32 - cd_offset;

    out.extend_from_slice(&EOCD_SIG.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // disk number
    out.extend_from_slice(&0u16.to_le_bytes()); // cd start disk
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&cd_size.to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // comment len
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zip::ZipArchive;

    #[test]
    fn crc32_known_vector() {
        // The canonical CRC-32 check value for "123456789".
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn written_zip_is_readable() {
        let entries = vec![
            ("[Content_Types].xml".to_string(), b"<types/>".to_vec()),
            ("word/document.xml".to_string(), b"<w:document/>".to_vec()),
        ];
        let bytes = write_zip(&entries);
        let arc = ZipArchive::open(&bytes).expect("our writer produces a readable ZIP");
        assert_eq!(arc.entries().len(), 2);
        assert_eq!(arc.read("word/document.xml").unwrap(), b"<w:document/>");
        assert_eq!(arc.read("[Content_Types].xml").unwrap(), b"<types/>");
    }

    #[test]
    fn stored_header_records_correct_crc() {
        let data = b"the quick brown fox".to_vec();
        let bytes = write_zip(&[("a.txt".to_string(), data.clone())]);
        // The CRC we wrote into the local header (offset 14) must equal crc32(data),
        // which is what a strict (CRC-validating) reader like Word checks.
        let stored_crc = u32::from_le_bytes(bytes[14..18].try_into().unwrap());
        assert_eq!(stored_crc, crc32(&data));
        let arc = ZipArchive::open(&bytes).expect("open");
        assert_eq!(arc.read("a.txt").unwrap(), data);
    }

    #[test]
    fn empty_archive() {
        let bytes = write_zip(&[]);
        let arc = ZipArchive::open(&bytes).expect("empty zip opens");
        assert_eq!(arc.entries().len(), 0);
    }
}
