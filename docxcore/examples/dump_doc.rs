//! Load a `.docx` and print its block structure + plain text — a quick way to
//! sanity-check a generated document round-trips through the loader.
//!
//! ```sh
//! cargo run -p docxcore --example dump_doc -- assets/sample.docx
//! ```

use docxcore::model::Block;

fn main() -> std::io::Result<()> {
    let path = std::env::args()
        .nth(1)
        .expect("usage: dump_doc <file.docx>");
    let bytes = std::fs::read(&path)?;
    let doc = docxcore::load::load(&bytes).expect("load failed");
    println!("{} blocks:", doc.body.len());
    for (i, b) in doc.body.iter().enumerate() {
        let kind = match b {
            Block::Paragraph(p) => format!(
                "Paragraph style={:?} heading={:?} num={:?}",
                p.props.style_id, p.props.heading_level, p.props.num_id
            ),
            Block::Table(t) => format!("Table {}x{}", t.rows.len(), t.grid.len()),
            Block::Raw(_) => "Raw".into(),
        };
        let text = b.plain_text();
        let text = text.trim();
        let preview: String = text.chars().take(60).collect();
        println!("  [{i}] {kind} | {preview}");
    }
    Ok(())
}
