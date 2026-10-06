//! Current Project resource identity and type, validated against XML probes.
use crate::tabledecode::{f64_at, u16_at, u32_at};
use projcore::Resource;
use projcore::model::ResourceType;
use std::collections::HashSet;

pub(crate) fn decode(bytes: &[u8], legacy: bool) -> Result<Option<Vec<Resource>>, String> {
    if legacy {
        return Ok(None);
    }
    let Some(table) = crate::tabledecode::read(bytes, "TBkndRsc", 37, 172, 0x0c40)? else {
        return Ok(None);
    };
    let mut result = Vec::new();
    let mut ids = HashSet::new();
    for row in &table.rows {
        // A moved/deleted resource may reuse its row ID; the stable UID is +4.
        let id = u32_at(row, 0);
        let uid = u32_at(row, 4);
        if uid == 0 {
            continue;
        } // Implicit "unassigned" resource, not in Project's XML resource sheet.
        if uid > i32::MAX as u32 || id > i32::MAX as u32 || !ids.insert(id) {
            return Err(format!("invalid or duplicate resource UID {uid} / ID {id}"));
        }
        // a2-material-cost-unassigned: +170 distinguishes work (0) from
        // material/cost (1); +166 is the standard rate format (8 for a
        // material per-unit rate, 2 otherwise) and can be stale: cement UID 2
        // in corpus/mpp/snapshots/35-resource-material.mpp has +166 = 2 with
        // key 299 = "bags" (paired 21-material-resource.mpp has +166 = 8), so
        // a (+170 = 1, +166 = 2) row is Material when key 299 (material label)
        // is non-empty, else Cost.
        let kind = match (u16_at(row, 170), u16_at(row, 166)) {
            (0, 2) => ResourceType::Work,
            (1, 8) => ResourceType::Material,
            (1, 2) => {
                if table
                    .field(uid, 299)
                    .is_some_and(|label| label.len() >= 2 && (label[0] != 0 || label[1] != 0))
                {
                    ResourceType::Material
                } else {
                    ResourceType::Cost
                }
            }
            other => return Err(format!("unknown resource type {other:?} for UID {uid}")),
        };
        let name = table
            .field(uid, 1)
            .map(|b| name(b, uid))
            .transpose()?
            .unwrap_or_default();
        let max_units = f64_at(row, 8) / 10000.0;
        if !max_units.is_finite() || max_units < 0.0 {
            return Err(format!("invalid max units for resource UID {uid}"));
        }
        result.push(Resource {
            uid: uid as i32,
            id: id as i32,
            name,
            kind,
            max_units,
            ..Resource::default()
        });
    }
    Ok(Some(result))
}

fn name(bytes: &[u8], uid: u32) -> Result<String, String> {
    if bytes.len() < 2 || !bytes.len().is_multiple_of(2) || bytes[bytes.len() - 2..] != [0, 0] {
        return Err(format!("invalid resource name for UID {uid}"));
    }
    let units: Vec<_> = bytes[..bytes.len() - 2]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|x| u16::from_le_bytes(*x))
        .collect();
    let value =
        String::from_utf16(&units).map_err(|_| format!("invalid resource name for UID {uid}"))?;
    if value.chars().any(char::is_control) {
        return Err(format!("invalid resource name for UID {uid}"));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cfb::{Node, write_cfb_tree};

    fn file(ids: [u32; 3], markers: [(u16, u16); 3], bad_name: bool) -> Vec<u8> {
        build(ids, markers, bad_name, None)
    }

    fn build(
        ids: [u32; 3],
        markers: [(u16, u16); 3],
        bad_name: bool,
        label: Option<(u32, &str)>,
    ) -> Vec<u8> {
        let mut fm = vec![0u8; 16];
        fm[..4].copy_from_slice(&[0xba, 0xad, 0xdf, 0xfa]);
        fm[8..12].copy_from_slice(&9u32.to_le_bytes());
        let mut fd = Vec::new();
        for i in 0..9 {
            let mut m = [0u8; 37];
            if i < 3 || i == 7 {
                m[..2].copy_from_slice(&4u16.to_le_bytes());
            } else if i == 8 {
                m[..2].copy_from_slice(&2u16.to_le_bytes());
            }
            m[4..8].copy_from_slice(&(fd.len() as u32).to_le_bytes());
            fm.extend(m);
            if i < 3 || i == 7 {
                fd.extend([0u8; 16]);
                continue;
            }
            let mut row = [0u8; 172];
            if i == 8 {
                row[..4].copy_from_slice(&ids[1].to_le_bytes());
                row[4..8].copy_from_slice(&2u32.to_le_bytes());
            } else if i > 3 {
                let j = i - 4;
                row[..4].copy_from_slice(&ids[j].to_le_bytes());
                row[4..8].copy_from_slice(&((j + 1) as u32).to_le_bytes());
                row[8..16].copy_from_slice(
                    &(if j == 1 { 20_000_000f64 } else { 10000f64 }).to_le_bytes(),
                );
                row[166..168].copy_from_slice(&markers[j].0.to_le_bytes());
                row[170..172].copy_from_slice(&markers[j].1.to_le_bytes());
            }
            fd.extend(row);
        }
        let mut vm = vec![0u8; 24];
        vm[..4].copy_from_slice(&[0xba, 0xad, 0xdf, 0xfa]);
        vm[8..12].copy_from_slice(&(3u32 + u32::from(label.is_some())).to_le_bytes());
        let mut v2 = Vec::new();
        for (i, name) in ["Worker", "Material", "Cost"].iter().enumerate() {
            let uid = (i + 1) as u32;
            vm.extend(uid.to_le_bytes());
            vm.extend((v2.len() as u32).to_le_bytes());
            vm.extend(1u16.to_le_bytes());
            vm.extend(0x0c40u16.to_le_bytes());
            let mut b: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
            if i != 0 || !bad_name {
                b.extend([0, 0]);
            }
            v2.extend((b.len() as u32).to_le_bytes());
            v2.extend(b);
        }
        if let Some((uid, text)) = label {
            // Key 299 is the UTF-16 material label ("bags" for cement UID 2 in
            // corpus/mpp/snapshots/35-resource-material.mpp).
            vm.extend(uid.to_le_bytes());
            vm.extend((v2.len() as u32).to_le_bytes());
            vm.extend(299u16.to_le_bytes());
            vm.extend(0x0c40u16.to_le_bytes());
            let mut b: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
            b.extend([0, 0]);
            v2.extend((b.len() as u32).to_le_bytes());
            v2.extend(b);
        }
        vm[20..24].copy_from_slice(&(v2.len() as u32).to_le_bytes());
        write_cfb_tree(&[Node::Storage(
            "   114",
            vec![Node::Storage(
                "TBkndRsc",
                vec![
                    Node::Stream("FixedMeta", fm),
                    Node::Stream("FixedData", fd),
                    Node::Stream("VarMeta", vm),
                    Node::Stream("Var2Data", v2),
                ],
            )],
        )])
    }

    #[test]
    fn decodes_work_material_cost_and_refuses_bad_resources() {
        let markers = [(2, 0), (8, 1), (2, 1)];
        let resources = decode(&file([10, 20, 30], markers, false), false)
            .unwrap()
            .unwrap();
        assert_eq!(
            resources
                .iter()
                .map(|r| (r.uid, r.id, r.kind))
                .collect::<Vec<_>>(),
            vec![
                (1, 10, ResourceType::Work),
                (2, 20, ResourceType::Material),
                (3, 30, ResourceType::Cost)
            ]
        );
        assert_eq!(resources[1].max_units, 2000.0);
        assert!(
            decode(&file([10, 10, 30], markers, false), false)
                .unwrap_err()
                .contains("duplicate")
        );
        assert!(
            decode(&file([10, 20, 30], [(2, 0), (9, 1), (2, 1)], false), false)
                .unwrap_err()
                .contains("unknown resource type")
        );
        assert!(
            decode(&file([10, 20, 30], markers, true), false)
                .unwrap_err()
                .contains("invalid resource name")
        );
    }

    #[test]
    fn material_with_label_and_stale_rate_format_is_material() {
        // (+170 = 1, +166 = 2) with a key-299 material label is Material:
        // cement UID 2 in corpus/mpp/snapshots/35-resource-material.mpp has
        // +166 = 2 (stale rate format; XML says StandardRateFormat 8) and
        // key 299 = "bags", and Project's XML names it Material.
        let markers = [(2, 0), (2, 1), (2, 1)];
        let resources = decode(
            &build([10, 20, 30], markers, false, Some((2, "bags"))),
            false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            resources
                .iter()
                .map(|r| (r.uid, r.kind))
                .collect::<Vec<_>>(),
            vec![
                (1, ResourceType::Work),
                (2, ResourceType::Material),
                (3, ResourceType::Cost)
            ]
        );
    }

    #[test]
    fn cost_without_label_stays_cost() {
        // (+170 = 1, +166 = 2) without a key-299 label stays Cost: travel
        // budget UID 3 in corpus/mpp/snapshots/36-resource-cost.mpp has
        // +166 = 2 and no key 299, and Project's XML names it Cost.
        let markers = [(2, 0), (2, 1), (2, 1)];
        let resources = decode(&file([10, 20, 30], markers, false), false)
            .unwrap()
            .unwrap();
        assert_eq!(
            resources
                .iter()
                .map(|r| (r.uid, r.kind))
                .collect::<Vec<_>>(),
            vec![
                (1, ResourceType::Work),
                (2, ResourceType::Cost),
                (3, ResourceType::Cost)
            ]
        );
    }

    #[test]
    fn empty_label_stays_cost() {
        // A key-299 entry whose payload is just the terminator (00 00) is an
        // empty label: the row stays Cost, like a label-less one.
        let markers = [(2, 0), (2, 1), (2, 1)];
        let resources = decode(&build([10, 20, 30], markers, false, Some((2, ""))), false)
            .unwrap()
            .unwrap();
        assert_eq!(
            resources
                .iter()
                .map(|r| (r.uid, r.kind))
                .collect::<Vec<_>>(),
            vec![
                (1, ResourceType::Work),
                (2, ResourceType::Cost),
                (3, ResourceType::Cost)
            ]
        );
    }
}
