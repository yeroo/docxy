//! Benchmarks for the calc engine and the `.xlsx` I/O path — the two hot
//! spots that dominate `xlsxy` startup and edit latency.
//!
//! Run with:  cargo bench -p gridcore --features bench

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use gridcore::engine::Engine;
use gridcore::sheet::{Cell, col_name};
use gridcore::xlsx::{load_xlsx, new_xlsx, save_xlsx};
use std::hint::black_box;

/// A workbook whose first column holds `n` numbers and whose second column
/// holds `n` running-sum formulas `=B{i-1}+A{i}` — a deep dependency chain
/// that forces the engine to walk the whole graph in order.
fn chain_workbook(n: u32) -> gridcore::xlsx::SheetPackage {
    let mut pkg = new_xlsx();
    let sheet = &mut pkg.workbook.sheets[0];
    for i in 0..n {
        sheet.set_cell(i, 0, Cell::number((i + 1) as f64));
        let f = if i == 0 {
            "A1".to_string()
        } else {
            format!("B{}+A{}", i, i + 1)
        };
        sheet.set_cell(i, 1, Cell::formula(&f));
    }
    pkg
}

/// A wide grid of independent `SUM` formulas over a shared data block —
/// exercises range evaluation and the spill/aggregation paths in parallel
/// columns rather than one long chain.
fn grid_workbook(rows: u32, cols: u32) -> gridcore::xlsx::SheetPackage {
    let mut pkg = new_xlsx();
    let sheet = &mut pkg.workbook.sheets[0];
    // Data block in columns A..C.
    for r in 0..rows {
        for c in 0..3 {
            sheet.set_cell(r, c, Cell::number((r * 3 + c + 1) as f64));
        }
    }
    // Formula block: each cell sums the three data cells on its row.
    for r in 0..rows {
        for c in 3..(3 + cols) {
            sheet.set_cell(
                r,
                c,
                Cell::formula(&format!("SUM(A{r1}:C{r1})", r1 = r + 1)),
            );
        }
    }
    pkg
}

fn bench_recalc_chain(c: &mut Criterion) {
    let mut g = c.benchmark_group("recalc_chain");
    for &n in &[500u32, 2000, 8000] {
        let pkg = chain_workbook(n);
        g.throughput(criterion::Throughput::Elements(n as u64));
        g.bench_with_input(BenchmarkId::from_parameter(n), &pkg, |b, pkg| {
            b.iter(|| {
                let mut wb = pkg.workbook.clone();
                let mut eng = Engine::new(&wb);
                eng.recalc_all(&mut wb);
                black_box(&wb);
            })
        });
    }
    g.finish();
}

fn bench_recalc_grid(c: &mut Criterion) {
    let pkg = grid_workbook(500, 20); // 10k formula cells
    c.bench_function("recalc_grid_500x20", |b| {
        b.iter(|| {
            let mut wb = pkg.workbook.clone();
            let mut eng = Engine::new(&wb);
            eng.recalc_all(&mut wb);
            black_box(&wb);
        })
    });
}

fn bench_incremental_edit(c: &mut Criterion) {
    // Steady-state: one cell edit rippling through a warm engine + graph.
    let pkg = chain_workbook(4000);
    let mut wb = pkg.workbook.clone();
    let mut eng = Engine::new(&wb);
    eng.recalc_all(&mut wb);
    let mut toggle = 0.0f64;
    c.bench_function("edit_head_of_4k_chain", |b| {
        b.iter(|| {
            toggle += 1.0;
            eng.set_cell(&mut wb, (0, 0, 0), Cell::number(black_box(toggle)));
            black_box(&wb);
        })
    });
}

/// The #878/#935 shape: many data seeds plus tall (`SUM(A:A)`-style) readers
/// down each column and wide (`SUM(1:1)`-style) readers across each row.
fn rect_readers_workbook(rows: u32, cols: u32) -> gridcore::xlsx::SheetPackage {
    let mut pkg = new_xlsx();
    let sheet = &mut pkg.workbook.sheets[0];
    for r in 0..rows {
        for c in 0..cols {
            sheet.set_cell(r, c, Cell::number((r * cols + c + 1) as f64));
        }
    }
    // One tall reader per data column, in the row after the data block.
    for c in 0..cols {
        let col = col_name(c);
        sheet.set_cell(
            rows + 1,
            c,
            Cell::formula(&format!("SUM({col}1:{col}{rows})")),
        );
    }
    // One wide reader per data row, in the column after the data block.
    let last = col_name(cols - 1);
    for r in 0..rows {
        sheet.set_cell(
            r,
            cols + 1,
            Cell::formula(&format!("SUM(A{r1}:{last}{r1})", r1 = r + 1)),
        );
    }
    pkg
}

/// The #948 plan-rect-both shape: a data block plus both-tall-and-wide
/// readers (`SUM(A1:T5000)`-style) whose dep rects cover the whole block.
fn block_readers_workbook(rows: u32, cols: u32) -> gridcore::xlsx::SheetPackage {
    let mut pkg = new_xlsx();
    let sheet = &mut pkg.workbook.sheets[0];
    for r in 0..rows {
        for c in 0..cols {
            sheet.set_cell(r, c, Cell::number((r * cols + c + 1) as f64));
        }
    }
    // 50 block readers in row 0, right of the data block (no overlap with it
    // or each other): identical `A1:{last}{rows}` dep rects, all covering
    // every data seed.
    let last = col_name(cols - 1);
    for c in cols + 1..=cols + 50 {
        sheet.set_cell(0, c, Cell::formula(&format!("SUM(A1:{last}{rows})")));
    }
    pkg
}

fn bench_edit_rect_readers(c: &mut Criterion) {
    // Steady-state: one data-cell edit at a time against tall/wide readers,
    // cycling through the data block so successive edits hit different seeds.
    let mut g = c.benchmark_group("edit_rect_readers");
    for (rows, cols) in [(1000u32, 10u32), (5000, 20)] {
        let pkg = rect_readers_workbook(rows, cols);
        let mut wb = pkg.workbook.clone();
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        let total = (rows * cols) as usize;
        let mut n = 0usize;
        g.bench_with_input(
            BenchmarkId::from_parameter(format!("{rows}x{cols}")),
            &(rows, cols),
            |b, _| {
                b.iter(|| {
                    let i = n % total;
                    n += 1;
                    let r = (i / cols as usize) as u32;
                    let c = (i % cols as usize) as u32;
                    eng.set_cell(&mut wb, (0, r, c), Cell::number(black_box(n as f64)));
                    black_box(&wb);
                });
            },
        );
    }
    g.finish();
}

fn bench_edit_block_readers(c: &mut Criterion) {
    // Steady-state: one data-cell edit at a time against block readers that
    // all cover the edited cell.
    let mut g = c.benchmark_group("edit_block_readers");
    for (rows, cols) in [(1000u32, 10u32), (5000, 20)] {
        let pkg = block_readers_workbook(rows, cols);
        let mut wb = pkg.workbook.clone();
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        let total = (rows * cols) as usize;
        let mut n = 0usize;
        g.bench_with_input(
            BenchmarkId::from_parameter(format!("{rows}x{cols}")),
            &(rows, cols),
            |b, _| {
                b.iter(|| {
                    let i = n % total;
                    n += 1;
                    let r = (i / cols as usize) as u32;
                    let c = (i % cols as usize) as u32;
                    eng.set_cell(&mut wb, (0, r, c), Cell::number(black_box(n as f64)));
                    black_box(&wb);
                });
            },
        );
    }
    g.finish();
}

fn bench_recalc_rect_readers(c: &mut Criterion) {
    let pkg = rect_readers_workbook(1000, 10);
    c.bench_function("recalc_rect_readers_1000x10", |b| {
        b.iter(|| {
            let mut wb = pkg.workbook.clone();
            let mut eng = Engine::new(&wb);
            eng.recalc_all(&mut wb);
            black_box(&wb);
        })
    });
}

fn bench_recalc_block_readers(c: &mut Criterion) {
    let pkg = block_readers_workbook(1000, 10);
    c.bench_function("recalc_block_readers_1000x10", |b| {
        b.iter(|| {
            let mut wb = pkg.workbook.clone();
            let mut eng = Engine::new(&wb);
            eng.recalc_all(&mut wb);
            black_box(&wb);
        })
    });
}

fn bench_xlsx_roundtrip(c: &mut Criterion) {
    let pkg = grid_workbook(300, 10);
    let mut wb = pkg.workbook.clone();
    let mut eng = Engine::new(&wb);
    eng.recalc_all(&mut wb);
    let pkg = {
        let mut p = pkg;
        p.workbook = wb;
        p
    };
    let bytes = save_xlsx(&pkg);
    c.bench_function("xlsx_save_300x10", |b| {
        b.iter(|| black_box(save_xlsx(black_box(&pkg))))
    });
    c.bench_function("xlsx_load_300x10", |b| {
        b.iter(|| black_box(load_xlsx(black_box(&bytes)).expect("valid xlsx")))
    });
}

fn bench_formula_parse(c: &mut Criterion) {
    // A mix representative of real sheets: refs, ranges, nesting, functions.
    let srcs: Vec<String> = (0..1000)
        .map(|i| {
            let col = col_name(i % 26);
            format!("IF(SUM({col}1:{col}100)>{i},VLOOKUP(A{i},$D$1:$F$50,3,0)*1.05,0)")
        })
        .collect();
    c.bench_function("parse_1000_mixed_formulas", |b| {
        b.iter(|| {
            for s in &srcs {
                let _ = black_box(Engine::validate(black_box(s)));
            }
        })
    });
}

criterion_group!(
    benches,
    bench_recalc_chain,
    bench_recalc_grid,
    bench_incremental_edit,
    bench_edit_rect_readers,
    bench_edit_block_readers,
    bench_recalc_rect_readers,
    bench_recalc_block_readers,
    bench_xlsx_roundtrip,
    bench_formula_parse
);
criterion_main!(benches);
