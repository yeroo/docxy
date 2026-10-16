//! Inspection API: the validated table readers behind
//! [`crate::project::project_from_mpp`], for tools and tests that look at
//! the `TBknd*` tables of a `.mpp` directly.
//!
//! The readers are the code the importer runs, re-exported as they are, so
//! none is a looser second reader, and the layout constants are the sizes
//! they check. Only [`var_entries`] and [`Table::fields`] are new for
//! inspection; [`var_entries`] checks a VarMeta header, count and value
//! bounds as the task reader does. Each
//! reader checks its counted index and refuses (`Err`) what it does not
//! recognise, so a tool sees exactly the rows the import would. Open the
//! container with [`crate::Cfb`] and read a table's streams by path
//! (`   114/TBkndTask/FixedMeta` in current Project, `   112/...` in
//! Project 2007).
//!
//! # Tables
//!
//! | Storage | Reader | Generations validated |
//! |---|---|---|
//! | `TBkndTask` | [`task_index`], [`task_fixed2`], [`decode_task_table`] | current Project, MPP12, MPP9 |
//! | `TBkndCons` | [`decode_task_table`] ([`LINK_RECORD_LEN`]-byte records) | current Project, MPP12, MPP9 |
//! | `TBkndRsc` | [`RESOURCE_TABLE`], [`decode_resources`] | current Project |
//! | `TBkndAssn` | [`ASSIGNMENT_TABLE`], [`decode_assignments`] | current Project |
//! | `TBkndCal` | [`decode_calendars`] | current Project |
//! | `Props` | [`props_entries`] | current Project |
//!
//! The FixedMeta streams these readers take start with the magic
//! `ba ad df fa` and the entry count `u32` at +8; each entry holds a kind
//! `u16` at +0 and the record's FixedData offset `u32` at +4. Kind 0 is a
//! live row, kind 2 a superseded one and kind 4 a schema stub or blank row.
//! The entry stride is per table: 47 bytes for tasks ([`TASK_META_STRIDE`]),
//! 37 for resources, 34 for assignments and 10 for calendars (the link
//! reader takes `TBkndCons/FixedData` alone). VarMeta entries are read by
//! [`var_entries`]; Var2Data blocks by the [`crate::vardata`] helpers
//! re-exported here.
//!
//! Current Project means the newest generation Project writes, stored under
//! `   114`; MPP12 is Project 2007 and MPP9 Project 2000-2003. The record
//! sizes and offsets each reader assumes are on its own item. A layout no
//! reader names is unvalidated: the readers refuse it or return `None`.

pub use crate::assndecode::decode as decode_assignments;
pub use crate::caldecode::{DecodedCalendars, decode as decode_calendars};
pub use crate::fixedmeta::{
    CurrentRecord, FIXED2_LEN, FIXED2_META_LEN, Fixed2, LegacyRecord, MPP12_TASK_RECORD_LEN,
    TASK_META_STRIDE, TASK_RECORD_LEN, TaskIndex, current_fixed2 as task_fixed2,
    index as task_index,
};
pub use crate::mpp::{
    MPP_EPOCH_DAYS, decode_checked_timestamp, decode_duration_minutes, decode_timestamp,
    working_duration_format,
};
pub use crate::props::{
    DEFAULT_CALENDAR_NAME, NEW_TASKS_ARE_MANUAL, PROJECT_START, SUBPROJECTS, WBS_CODE_MASK,
    entries as props_entries,
};
pub use crate::rscdecode::decode as decode_resources;
pub use crate::tabledecode::{
    ASSIGNMENT_TABLE, RESOURCE_TABLE, Table, TableLayout, f64_at, i32_at, present as table_present,
    read as read_table, u16_at, u32_at,
};
pub use crate::taskdecode::{
    LINK_RECORD_LEN, Table as TaskTable, cost_rate, decode_table as decode_task_table, decode_text,
    tenths_to_minutes, work_minutes,
};
pub use crate::vardata::{block_offsets, blocks, string_at, strings, utf16le_string};
pub use crate::varmeta::{VarEntry, var_entries};
