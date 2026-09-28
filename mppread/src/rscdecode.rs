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
    for (i, row) in table.rows.iter().enumerate() {
        if i < 3 {
            continue;
        } // Three schema records, including UID 0's predecessor.
        let uid = u32_at(row, 0);
        let id = u32_at(row, 4);
        if uid == 0 {
            continue;
        } // Implicit "unassigned" resource, not in Project's XML resource sheet.
        if uid > i32::MAX as u32 || id > i32::MAX as u32 || !ids.insert(id) {
            return Err(format!("invalid or duplicate resource UID {uid} / ID {id}"));
        }
        // a2-material-cost-unassigned: +170 distinguishes work (0) from
        // material/cost (1); +166 is 8 for material, 2 for cost.
        let kind = match (u16_at(row, 170), u16_at(row, 166)) {
            (0, _) => ResourceType::Work,
            (1, 8) => ResourceType::Material,
            (1, 2) => ResourceType::Cost,
            other => return Err(format!("unknown resource type {other:?} for UID {uid}")),
        };
        let name = table
            .fields
            .get(&(uid, 1))
            .ok_or_else(|| format!("missing resource name for UID {uid}"))?;
        let max_units = f64_at(row, 8) / 10000.0;
        if !max_units.is_finite() || !(0.0..=1000.0).contains(&max_units) {
            return Err(format!("invalid max units for resource UID {uid}"));
        }
        result.push(Resource {
            uid: uid as i32,
            id: id as i32,
            name: crate::tabledecode::name(name, uid)?,
            kind,
            max_units,
            ..Resource::default()
        });
    }
    Ok(Some(result))
}
