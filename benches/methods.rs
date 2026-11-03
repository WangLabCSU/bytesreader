//! Compare standard Read methods on BytesReader and std::io::BufReader.
//! Run with `cargo bench --bench methods`; add `--features nightly` on nightly
//! Rust to enable BytesReader's read_buf path.

use std::fs::{self, File};
use std::hint::black_box;
use std::io::{self, BufReader, IoSliceMut, Read};
use std::path::{Path, PathBuf};
use std::time::Instant;

use bytesreader::BytesReader;

#[path = "src/table.rs"]
mod table;

const LEN: usize = 4 * 1024 * 1024;
const CAP: usize = 8192;
const PASSES: usize = 8;

struct Defaults<R>(R);
impl<R: Read> Read for Defaults<R> {
    #[inline]
    fn read(&mut self, dst: &mut [u8]) -> io::Result<usize> {
        self.0.read(dst)
    }
}

fn run<R: Read>(mut reader: R, mode: usize) -> io::Result<usize> {
    match mode {
        0 => {
            let mut dst = [0; 32];
            for _ in 0..LEN / 32 {
                reader.read_exact(&mut dst)?;
                black_box(&dst);
            }
            assert_eq!(reader.read(&mut dst)?, 0);
            Ok(LEN)
        }
        1 => {
            let mut a = [0; 32];
            let mut b = [0; 32];
            let mut len = 0;
            loop {
                let n = reader
                    .read_vectored(&mut [IoSliceMut::new(&mut a), IoSliceMut::new(&mut b)])?;
                if n == 0 {
                    break;
                }
                black_box((&a, &b, n));
                len += n;
            }
            Ok(len)
        }
        2 => {
            let mut dst = Vec::with_capacity(LEN);
            let n = reader.read_to_end(&mut dst)?;
            black_box(&dst);
            Ok(n)
        }
        _ => {
            let mut dst = String::with_capacity(LEN);
            let n = reader.read_to_string(&mut dst)?;
            black_box(&dst);
            Ok(n)
        }
    }
}

fn measure(path: &Path, implementation: usize, mode: usize) -> f64 {
    let start = Instant::now();
    for _ in 0..PASSES {
        let file = File::open(path).unwrap();
        let n = match implementation {
            0 => run(Defaults(BytesReader::with_capacity(CAP, file)), mode),
            1 => run(BytesReader::with_capacity(CAP, file), mode),
            _ => run(BufReader::with_capacity(CAP, file), mode),
        }
        .unwrap();
        assert_eq!(n, LEN);
    }
    start.elapsed().as_secs_f64() * 1e3 / PASSES as f64
}

fn main() {
    struct Input(PathBuf);
    impl Drop for Input {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }
    let input_file = Input(
        std::env::temp_dir().join(format!("bytesreader-read-methods-{}", std::process::id())),
    );
    let path = input_file.0.as_path();
    let input: Vec<u8> = (0..LEN).map(|n| b'a' + (n % 26) as u8).collect();
    fs::write(path, &input).unwrap();
    // Check full content outside timing for every specialized/default helper.
    for implementation in 0..3 {
        for mode in 0..4 {
            let file = File::open(path).unwrap();
            let mut reader: Box<dyn Read> = match implementation {
                0 => Box::new(Defaults(BytesReader::with_capacity(CAP, file))),
                1 => Box::new(BytesReader::with_capacity(CAP, file)),
                _ => Box::new(BufReader::with_capacity(CAP, file)),
            };
            let mut actual = Vec::new();
            match mode {
                0 => {
                    actual.resize(LEN, 0);
                    for dst in actual.chunks_mut(32) {
                        reader.read_exact(dst).unwrap();
                    }
                }
                1 => loop {
                    let mut a = [0; 32];
                    let mut b = [0; 32];
                    let n = reader
                        .read_vectored(&mut [IoSliceMut::new(&mut a), IoSliceMut::new(&mut b)])
                        .unwrap();
                    if n == 0 {
                        break;
                    }
                    actual.extend_from_slice(&a[..n.min(32)]);
                    actual.extend_from_slice(&b[..n.saturating_sub(32)]);
                },
                2 => {
                    reader.read_to_end(&mut actual).unwrap();
                }
                _ => {
                    let mut text = String::new();
                    reader.read_to_string(&mut text).unwrap();
                    actual = text.into_bytes();
                }
            }
            assert_eq!(actual, input);
        }
    }
    let mut rows = Vec::new();
    for mode in 0..4 {
        for _ in 0..2 {
            for implementation in 0..3 {
                black_box(measure(path, implementation, mode));
            }
        }
        let mut samples: [Vec<f64>; 3] = std::array::from_fn(|_| Vec::new());
        for round in 0..12 {
            let order = std::array::from_fn::<_, 3, _>(|i| (i + round) % 3);
            for implementation in order {
                samples[implementation].push(measure(path, implementation, mode));
            }
        }
        for samples in &mut samples {
            samples.sort_by(f64::total_cmp);
        }
        let medians = samples
            .each_ref()
            .map(|values| (values[5] + values[6]) / 2.0);
        let fastest = medians.into_iter().fold(f64::INFINITY, f64::min);
        for (implementation, samples) in samples.into_iter().enumerate() {
            let median = medians[implementation];
            rows.push(table::Row {
                fastest: median == fastest,
                subgroup: None,
                cells: [
                    [
                        "BytesReader using Read defaults",
                        "BytesReader",
                        "std::io::BufReader",
                    ][implementation]
                        .to_owned(),
                    [
                        "Read::read_exact (32 bytes)",
                        "Read::read_vectored (2 x 32 bytes)",
                        "Read::read_to_end",
                        "Read::read_to_string",
                    ][mode]
                        .to_owned(),
                    format!("{median:.4}"),
                    format!("{:.4}", samples[0]),
                    format!("{:.4}", samples[11]),
                    format!("{:.1}", 4000.0 / median),
                ],
            });
        }
    }
    // Keep each reader's methods together; timing order is unchanged.
    rows.sort_by_key(|row| match row.cells[0].as_str() {
        "BytesReader" => 0,
        "BytesReader using Read defaults" => 1,
        _ => 2,
    });
    table::text("Standard Read methods: 4 MiB file (OS cache), 8192 B input buffer");
    for group in rows.chunk_by(|a, b| a.cells[0] == b.cells[0]) {
        let compact: Vec<_> = group
            .iter()
            .map(|row| {
                let [_, call, median, min, max, throughput] = &row.cells;
                table::Row {
                    cells: [
                        call.clone(),
                        median.clone(),
                        min.clone(),
                        max.clone(),
                        throughput.clone(),
                    ],
                    fastest: row.fastest,
                    subgroup: None,
                }
            })
            .collect();
        table::print(
            &format!("{} | time: ms/file", group[0].cells[0]),
            ["call", "median", "min", "max", "MiB/s"],
            &compact,
            1,
            1,
        );
    }
    table::text(
        "Time: milliseconds per file (lower is better); MiB/s: throughput (higher is better). Green marks the fastest median for the same method.",
    );
    table::text(
        "Read defaults: a wrapper forwards only read(); other methods use Read's default implementations.",
    );
    table::text(
        "12 timed rounds, 8 full passes per round, after two warm-ups; order rotates. Includes opening, reading and output allocation; verifies contents before timing.",
    );
}
