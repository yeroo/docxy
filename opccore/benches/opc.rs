//! Microbenchmarks for the container hot paths shared by every OOXML format:
//! raw DEFLATE decompression and STORED-entry ZIP round-tripping.
//!
//! Run with:  cargo bench -p opccore --features bench

use criterion::{Criterion, criterion_group, criterion_main};
use opccore::inflate::inflate_raw;
use opccore::zip::ZipArchive;
use opccore::zipwrite::write_zip;
use std::hint::black_box;

/// A ~140 KB raw-DEFLATE stream of realistic (varied) OOXML text, and its
/// decompressed size — captured offline so the bench is hermetic.
const DEFLATED: &[u8] = include_bytes!("fixtures/sample.deflate");
const RAW_SIZE: usize = include!("fixtures/sample.size");

fn bench_inflate(c: &mut Criterion) {
    let mut g = c.benchmark_group("inflate");
    g.throughput(criterion::Throughput::Bytes(RAW_SIZE as u64));
    g.bench_function("raw_deflate_140k", |b| {
        b.iter(|| {
            let out = inflate_raw(black_box(DEFLATED), RAW_SIZE).expect("valid deflate");
            black_box(out.len())
        })
    });
    g.finish();
}

fn bench_zip_roundtrip(c: &mut Criterion) {
    // Build a small multi-part package once, then measure open + read-all.
    let payload = b"<xml>".repeat(2000);
    let entries: Vec<(String, Vec<u8>)> = (0..12)
        .map(|i| (format!("part{i}.xml"), payload.clone()))
        .collect();
    let bytes = write_zip(&entries);

    c.bench_function("zip_open_and_read_all", |b| {
        b.iter(|| {
            let arc = ZipArchive::open(black_box(&bytes)).expect("valid zip");
            let mut total = 0usize;
            for e in arc.entries() {
                total += arc.extract(e).map_or(0, |v| v.len());
            }
            black_box(total)
        })
    });
}

criterion_group!(benches, bench_inflate, bench_zip_roundtrip);
criterion_main!(benches);
