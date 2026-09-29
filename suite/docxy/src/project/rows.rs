//! The rows the entry table draws, top to bottom, for the UI harness. A
//! collapsed summary hides its subtree, so the harness's A1 rows are positions
//! among the drawn rows, not task indexes; every harness path that turns a row
//! into a task goes through [`shown_task`] so they cannot disagree.
use super::*;
use ctlcore::json::Json;

/// What one drawn row of the entry table holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ShownRow {
    /// The task at this index of `tasks`.
    Task(usize),
    /// The empty entry row just below the last drawn task.
    Entry,
}

/// The zero-based drawn row `row`: a task, the entry row just below the last
/// one, or `None` past it.
pub(crate) fn shown_task(ed: &ProjectEditor, row: usize) -> Option<ShownRow> {
    let rows = ed.visible_rows();
    match rows.get(row) {
        Some(&i) => Some(ShownRow::Task(i)),
        None if row == rows.len() => Some(ShownRow::Entry),
        None => None,
    }
}

/// The zero-based drawn row of task `index`; `None` while a collapsed summary
/// hides it.
pub(crate) fn shown_row_of(ed: &ProjectEditor, index: usize) -> Option<usize> {
    ed.visible_rows().iter().position(|&i| i == index)
}

/// A Project cell a harness verb addresses, by A1 drawn row or by `{uid, column}`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CellTarget {
    /// The zero-based drawn row; `None` for a task a collapsed summary hides.
    pub row: Option<usize>,
    /// What that row holds; `None` past the entry row.
    pub at: Option<ShownRow>,
    /// The column, unchecked for an A1 reference so each verb keeps its own
    /// out-of-table message.
    pub col: usize,
}

/// `{cell: "C3"}` (the third drawn row) or `{uid, column}` (a task whether
/// drawn or not; `column` is a zero-based index or a header name).
pub(crate) fn project_cell_target(args: &Json, ed: &ProjectEditor) -> Result<CellTarget, String> {
    let Some(uid) = args.get("uid") else {
        let (r, c) = harness::cell_arg(args, "cell")?;
        let row = r as usize;
        return Ok(CellTarget {
            row: Some(row),
            at: shown_task(ed, row),
            col: c as usize,
        });
    };
    if args.get("cell").is_some() {
        return Err("give either 'cell' or 'uid', not both".into());
    }
    let uid = uid
        .as_i64()
        .and_then(|n| i32::try_from(n).ok())
        .ok_or("'uid' must be a whole number")?;
    let col = column_arg(args)?;
    let index = ed
        .project()
        .tasks
        .iter()
        .position(|t| t.uid == uid)
        .ok_or_else(|| format!("no task with UID {uid}"))?;
    Ok(CellTarget {
        row: shown_row_of(ed, index),
        at: Some(ShownRow::Task(index)),
        col,
    })
}

/// A required `column`: a zero-based index or one of [`COLUMNS`], in any case.
fn column_arg(args: &Json) -> Result<usize, String> {
    let col = match args.get("column") {
        None => return Err("missing argument 'column'".into()),
        Some(Json::Str(name)) => COLUMNS
            .iter()
            .position(|c| c.eq_ignore_ascii_case(name.trim()))
            .ok_or_else(|| {
                format!(
                    "no Project column named '{name}' (one of {})",
                    COLUMNS.join(", ")
                )
            })?,
        Some(v) => v
            .as_usize()
            .ok_or("'column' must be a column index or a column name")?,
    };
    if col >= COLUMN_COUNT {
        return Err("No Project column at this index".into());
    }
    Ok(col)
}

/// The `rows` reply: the drawn task rows, top to bottom, without the entry row.
/// View, table, filter, group and sort are Project's names for the only ones
/// the tab has today.
pub(crate) fn rows_json(ed: &ProjectEditor) -> Json {
    let tasks = &ed.project().tasks;
    let rows: Vec<Json> = ed
        .visible_rows()
        .into_iter()
        .enumerate()
        .map(|(row, i)| row_json(ed, &tasks[i], row + 1))
        .collect();
    Json::obj(vec![
        ("view", Json::Str("Gantt Chart".into())),
        ("table", Json::Str("Entry".into())),
        ("filter", Json::Str("All Tasks".into())),
        ("group", Json::Str("No Group".into())),
        ("sort", Json::Str("ID".into())),
        ("count", Json::Num(rows.len() as f64)),
        ("total", Json::Num(tasks.len() as f64)),
        ("rows", Json::Arr(rows)),
    ])
}

fn row_json(ed: &ProjectEditor, task: &Task, row: usize) -> Json {
    let blank = task.is_null;
    let summary = !blank && task.summary;
    let kind = if blank {
        "blank"
    } else if task.external_task == Some(true) {
        "external"
    } else if summary {
        "summary"
    } else {
        "task"
    };
    let cells = project_row(ed, task).into_iter().map(Json::Str).collect();
    Json::obj(vec![
        ("row", Json::Num(row as f64)),
        ("kind", Json::Str(kind.into())),
        ("id", Json::Num(f64::from(task.id))),
        ("uid", Json::Num(f64::from(task.uid))),
        (
            "name",
            Json::Str(if blank {
                String::new()
            } else {
                task.name.clone()
            }),
        ),
        (
            "level",
            if blank {
                Json::Null
            } else {
                Json::Num(f64::from(task.outline_level))
            },
        ),
        ("summary", Json::Bool(summary)),
        (
            "collapsed",
            Json::Bool(summary && ed.is_collapsed(task.uid)),
        ),
        ("blank", Json::Bool(blank)),
        ("cells", Json::Arr(cells)),
    ])
}

#[cfg(test)]
mod tests;
