//! Benchmarks for the DOCX pipeline: parse bytes → model, serialize model →
//! XML, render to a terminal grid, and export to PDF.
//!
//! Run with:  cargo bench -p docxcore --features bench

use criterion::{Criterion, criterion_group, criterion_main};
use docxcore::export::{PdfOptions, to_pdf};
use docxcore::load::load;
use docxcore::package::{load_package, save_package};
use docxcore::render::{RenderOptions, render};
use docxcore::serialize::document_to_xml;
use std::hint::black_box;

const SAMPLE: &[u8] = include_bytes!("../tests/fixtures/sample.docx");

fn bench_load(c: &mut Criterion) {
    c.bench_function("docx_load", |b| {
        b.iter(|| black_box(load(black_box(SAMPLE)).expect("valid docx")))
    });
}

fn bench_serialize(c: &mut Criterion) {
    let doc = load(SAMPLE).expect("valid docx");
    c.bench_function("docx_document_to_xml", |b| {
        b.iter(|| black_box(document_to_xml(black_box(&doc))))
    });
}

fn bench_package_roundtrip(c: &mut Criterion) {
    // Full container round-trip: unzip+parse all parts, then re-zip.
    c.bench_function("docx_package_load", |b| {
        b.iter(|| black_box(load_package(black_box(SAMPLE)).expect("valid package")))
    });
    let pkg = load_package(SAMPLE).expect("valid package");
    c.bench_function("docx_package_save", |b| {
        b.iter(|| black_box(save_package(black_box(&pkg))))
    });
}

fn bench_render(c: &mut Criterion) {
    let doc = load(SAMPLE).expect("valid docx");
    let opts = RenderOptions::default();
    c.bench_function("docx_render_80col", |b| {
        b.iter(|| black_box(render(black_box(&doc), black_box(&opts))))
    });
}

fn bench_pdf(c: &mut Criterion) {
    let doc = load(SAMPLE).expect("valid docx");
    let opts = PdfOptions::default();
    c.bench_function("docx_to_pdf", |b| {
        b.iter(|| black_box(to_pdf(black_box(&doc), black_box(&opts))))
    });
}

criterion_group!(
    benches,
    bench_load,
    bench_serialize,
    bench_package_roundtrip,
    bench_render,
    bench_pdf
);
criterion_main!(benches);
