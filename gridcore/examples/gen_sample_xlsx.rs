//! Generate `assets/sample.xlsx` — a small showcase workbook for trying
//! xlsxy (`cargo run -p gridcore --example gen_sample_xlsx`).

use gridcore::engine::Engine;
use gridcore::sheet::{Cell, CellValue};
use gridcore::xlsx::{new_xlsx, save_xlsx};

fn main() {
    let mut pkg = new_xlsx();
    {
        let s = &mut pkg.workbook.sheets[0];
        s.name = "Budget".to_string();
        for (i, h) in ["Item", "Qty", "Unit price", "Total"].iter().enumerate() {
            s.set_cell(0, i as u32, Cell::text(h));
        }
        let rows = [
            ("Laptop", 2.0, 1199.0),
            ("Monitor", 4.0, 249.5),
            ("Keyboard", 6.0, 39.99),
            ("Dock", 2.0, 179.0),
        ];
        for (i, (item, qty, price)) in rows.iter().enumerate() {
            let r = i as u32 + 1;
            s.set_cell(r, 0, Cell::text(item));
            s.set_cell(r, 1, Cell::number(*qty));
            s.set_cell(r, 2, Cell::number(*price));
            s.set_cell(r, 3, Cell::formula(&format!("B{0}*C{0}", r + 1)));
        }
        s.set_cell(6, 0, Cell::text("Grand total"));
        s.set_cell(6, 3, Cell::formula("SUM(D2:D5)"));
        s.set_cell(7, 0, Cell::text("Average line"));
        s.set_cell(7, 3, Cell::formula("AVERAGE(D2:D5)"));
        s.set_cell(8, 0, Cell::text("Over $500?"));
        s.set_cell(
            8,
            3,
            Cell::formula("IF(D7>500,\"yes — review\",\"within budget\")"),
        );
        s.set_col_width(0, 14.0);
        s.set_col_width(2, 11.0);
        s.set_col_width(3, 12.0);
    }

    // Compute the cached values so Excel (and xlsxy) open a ready workbook.
    let mut engine = Engine::new(&pkg.workbook);
    engine.recalc_all(&mut pkg.workbook);

    let grand = pkg.workbook.sheets[0]
        .cell(6, 3)
        .map(|c| c.value.clone())
        .unwrap_or(CellValue::Empty);
    let bytes = save_xlsx(&pkg);
    std::fs::create_dir_all("assets").expect("mkdir assets");
    std::fs::write("assets/sample.xlsx", &bytes).expect("write sample.xlsx");
    println!(
        "wrote assets/sample.xlsx ({} bytes), grand total = {grand:?}",
        bytes.len()
    );
}
