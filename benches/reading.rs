//! Compare complete LF-delimited Items using identical input and initial buffer
//! capacities. Small, medium, and large groups use 20-byte steps up to
//! 500 B, 1000 B, and 1500 B respectively, including LF.
//! A fourth read-only workload uses Items larger than the initial buffer.
//! Run with `cargo bench --bench reading`; add `--features nightly` on nightly
//! Rust to use Read::read_buf.
//! Pass `--summary=no` to measure each payload size separately on file input.

#![cfg_attr(feature = "nightly", feature(core_io_borrowed_buf, read_buf))]

use std::fs::OpenOptions;
use std::hint::black_box;
#[cfg(feature = "nightly")]
use std::io::BorrowedBuf;
use std::io::{self, BufRead, BufReader, Read, Seek, Write};
use std::mem;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use bytes::{Buf, BytesMut};
use bytesreader::BytesReader;
use memchr::memchr;

#[path = "src/table.rs"]
mod table;

const CAPACITY: usize = 8 << 10;
// One complete rotation gives every method each timing position once.
const SAMPLES: usize = DEFAULT_MODES.len();
const ROUNDS: usize = 4;
const DEFAULT_MODES: [usize; 13] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
const ALIGNED_MODES: [usize; 19] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18,
];
const LIMITED_MODES: [usize; 6] = [13, 14, 15, 16, 17, 18];
// Reader, complete-record operation, and handling when a record spans reads.
// This describes the benchmark loops; it does not limit the reader APIs.
const METHODS: [(&str, &str, &str); 19] = [
    (
        "BytesReader",
        "take_bytes(..end, None)",
        "Keep the unfinished record in the input buffer and read more bytes after it. Grow the buffer if space runs out.",
    ),
    (
        "BytesReader",
        "take_view(..end, None)",
        "Keep the unfinished record in the input buffer and read more bytes after it. Grow the buffer if space runs out.",
    ),
    (
        "BytesReader",
        "take_view(..end, None).to_vec()",
        "Keep the unfinished record in the input buffer and read more bytes after it. Grow the buffer if space runs out.",
    ),
    (
        "std::io::BufReader",
        "fill_buf() + &buf[..end] + consume(end)",
        "Copy the parts of the record into a temporary Vec, then process a slice of that Vec.",
    ),
    (
        "std::io::BufReader",
        "fill_buf() + buf[..end].to_vec() + consume(end)",
        "Copy the parts into a temporary Vec, copy the completed record into the result Vec, and reuse the temporary Vec for the next record.",
    ),
    (
        "std::io::BufReader",
        "read_until(b'\\n', &mut Vec::new())",
        "read_until copies data from successive reads into the result Vec until it finds the newline.",
    ),
    (
        "BytesMut",
        "&buf[..end] + advance(end)",
        "Keep the unfinished record in the input buffer and read more bytes after it. Grow the buffer if space runs out.",
    ),
    (
        "BytesMut",
        "buf[..end].to_vec() + advance(end)",
        "Keep the unfinished record in the input buffer and read more bytes after it. Grow the buffer if space runs out.",
    ),
    (
        "BytesMut",
        "split_to(end)",
        "Keep the unfinished record in the input buffer and read more bytes after it. Grow the buffer if space runs out.",
    ),
    (
        "BytesReader",
        "fill_buf() + buf[..end].to_vec() + consume(end)",
        "Copy the parts into a temporary Vec, then transfer that Vec as the result without another copy.",
    ),
    (
        "BytesReader",
        "fill_buf() + buf[..end].to_vec() + consume(end)",
        "Copy the parts into a temporary Vec, copy the completed record into the result Vec, and reuse the temporary Vec for the next record.",
    ),
    (
        "std::io::BufReader",
        "fill_buf() + buf[..end].to_vec() + consume(end)",
        "Copy the parts into a temporary Vec, then transfer that Vec as the result without another copy.",
    ),
    (
        "std::io::BufReader",
        "fill_buf() + BytesMut::from(&buf[..end]) + consume(end)",
        "Copy the parts into a temporary Vec, then copy the completed record into a BytesMut result.",
    ),
    (
        "std::io::BufReader",
        "fill_buf() + &buf[..end] + consume(end)",
        "Stop if a read ends inside a record. This test cannot join data from separate reads.",
    ),
    (
        "std::io::BufReader",
        "fill_buf() + buf[..end].to_vec() + consume(end)",
        "Stop if a read ends inside a record. This test cannot join data from separate reads.",
    ),
    (
        "BytesReader",
        "fill_buf() + &buf[..end] + consume(end)",
        "Stop if a read ends inside a record. This test cannot join data from separate reads.",
    ),
    (
        "BytesReader",
        "fill_buf() + buf[..end].to_vec() + consume(end)",
        "Stop if a read ends inside a record. This test cannot join data from separate reads.",
    ),
    (
        "BytesReader",
        "take_view(..end, None)",
        "Stop if a read ends inside a record. This test cannot join data from separate reads.",
    ),
    (
        "BytesReader",
        "take_view(..end, None).to_vec()",
        "Stop if a read ends inside a record. This test cannot join data from separate reads.",
    ),
];

// Establish Item groups and reader order. Footnote variants are grouped before
// printing; measurement order still rotates independently.
const DISPLAY_ORDER: [usize; 19] = [
    1, 15, 17, 3, 13, 6, 2, 9, 10, 16, 18, 4, 11, 14, 5, 7, 0, 12, 8,
];

// Use the same LF search and include LF in every returned frame.
#[inline(always)]
fn find_line_end(src: &[u8]) -> Option<usize> {
    memchr(b'\n', src).map(|at| at + 1)
}

#[inline(always)]
fn copy_frame(frame: &[u8]) -> Vec<u8> {
    frame.to_vec()
}

// Shared by the BytesReader and BufReader accumulation paths. Complete buffered lines require
// one copy. Partial lines either transfer the accumulated Vec or copy it and
// retain scratch capacity, according to the same REUSE choice on both paths.
#[inline(always)]
fn finish_owned<const REUSE: bool>(partial: &mut Vec<u8>, frame: &[u8]) -> Vec<u8> {
    if partial.is_empty() {
        copy_frame(frame)
    } else {
        partial.extend_from_slice(frame);
        if REUSE {
            let item = copy_frame(partial);
            partial.clear();
            item
        } else {
            mem::take(partial)
        }
    }
}

// Remember how much of the pending frame has already been searched for LF.
#[derive(Default)]
struct Lines(usize);
impl Lines {
    // Share this scanner with the direct BytesMut baseline so only buffering
    // and frame delivery differ, not the LF search or partial-line bookkeeping.
    #[inline(always)]
    fn line_end(&mut self, src: &[u8]) -> Option<usize> {
        // A new frame starts at zero. Keep offset handling on the partial-frame
        // path so complete frames do not repeatedly reset an already-zero scan.
        if self.0 != 0 {
            return self.line_end_partial(src);
        }
        if let Some(end) = find_line_end(src) {
            Some(end)
        } else {
            self.0 = src.len();
            None
        }
    }

    // Inline so the framing loop can simplify the returned decision's bounds.
    #[inline(always)]
    fn line_end_partial(&mut self, src: &[u8]) -> Option<usize> {
        // Previously searched bytes remain in src across reads. Search only
        // the new suffix, including when a frame spans several reads.
        if let Some(end) = find_line_end(&src[self.0..]) {
            let end = self.0 + end;
            self.0 = 0;
            Some(end)
        } else {
            self.0 = src.len();
            None
        }
    }
}
// A caller-owned parsing loop. It keeps offsets across reads and decides
// whether a partial record at EOF is an error.
#[inline(always)]
fn next_end<R: Read>(reader: &mut BytesReader<R>, lines: &mut Lines) -> io::Result<Option<usize>> {
    loop {
        if let Some(end) = lines.line_end(reader.buffer()) {
            return Ok(Some(end));
        }
        if reader.read_more()? == 0 {
            return if reader.buffer().is_empty() {
                Ok(None)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "incomplete line",
                ))
            };
        }
    }
}

fn next_accumulated<R: BufRead, const REUSE: bool>(
    reader: &mut R,
    partial: &mut Vec<u8>,
) -> io::Result<Option<Vec<u8>>> {
    loop {
        let buf = reader.fill_buf()?;
        if buf.is_empty() {
            return if partial.is_empty() {
                Ok(None)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "incomplete line",
                ))
            };
        }
        if let Some(end) = find_line_end(buf) {
            let item = finish_owned::<REUSE>(partial, &buf[..end]);
            reader.consume(end);
            return Ok(Some(item));
        }
        partial.extend_from_slice(buf);
        let len = buf.len();
        reader.consume(len);
    }
}

// Every path presents the same full frame, including LF, to the consumer.
fn observe<const VERIFY: bool>(frame: &[u8], totals: &mut (usize, usize)) {
    if VERIFY {
        assert_eq!(frame.last(), Some(&b'\n'));
        for (offset, &byte) in frame[..frame.len() - 1].iter().enumerate() {
            assert_eq!(byte, ((totals.1 + offset) % 245 + 11) as u8);
        }
    }
    black_box(frame);
    totals.0 += 1;
    totals.1 += frame.len();
}

#[inline(never)]
fn buffered<R: Read, const MODE: usize, const VERIFY: bool>(
    source: R,
    capacity: usize,
) -> (usize, usize) {
    let mut reader = BytesReader::with_capacity(capacity, source);
    let mut lines = Lines::default();
    let mut totals = (0, 0);
    while let Some(end) = next_end(&mut reader, &mut lines).unwrap() {
        match MODE {
            0 => observe::<VERIFY>(&reader.take_bytes(..end, None), &mut totals),
            1 => observe::<VERIFY>(reader.take_view(..end, None), &mut totals),
            2 => {
                let item = copy_frame(reader.take_view(..end, None));
                observe::<VERIFY>(&item, &mut totals);
            }
            _ => unreachable!(),
        }
    }
    totals
}

#[inline(never)]
fn accumulating<R: Read, const REUSE: bool, const VERIFY: bool>(
    source: R,
    capacity: usize,
) -> (usize, usize) {
    let mut reader = BytesReader::with_capacity(capacity, source);
    let mut partial = Vec::new();
    let mut totals = (0, 0);
    while let Some(item) = next_accumulated::<_, REUSE>(&mut reader, &mut partial).unwrap() {
        observe::<VERIFY>(&item, &mut totals);
    }
    totals
}

// MODE 0 borrows complete frames; MODE 1 copies them with to_vec(); MODE 2
// uses read_until with a fresh Vec. REUSE controls the same partial-line
// completion strategy as accumulating() for MODE 1.
#[inline(never)]
fn standard<R: Read, const MODE: usize, const REUSE: bool, const VERIFY: bool>(
    source: R,
    capacity: usize,
) -> (usize, usize) {
    let mut reader = BufReader::with_capacity(capacity, source);
    let mut totals = (0, 0);
    if MODE == 2 {
        loop {
            let mut item = Vec::new();
            if reader.read_until(b'\n', &mut item).unwrap() == 0 {
                break;
            }
            observe::<VERIFY>(&item, &mut totals);
        }
    } else {
        let mut scratch = Vec::new();
        loop {
            let buf = reader.fill_buf().unwrap();
            if buf.is_empty() {
                assert!(scratch.is_empty());
                break;
            }
            if let Some(end) = find_line_end(buf) {
                if MODE == 1 {
                    let item = finish_owned::<REUSE>(&mut scratch, &buf[..end]);
                    observe::<VERIFY>(&item, &mut totals);
                } else if MODE == 3 {
                    // BufReader cannot transfer its storage: return the same
                    // owned BytesMut type by copying the complete record.
                    let item = if scratch.is_empty() {
                        BytesMut::from(&buf[..end])
                    } else {
                        scratch.extend_from_slice(&buf[..end]);
                        let item = BytesMut::from(scratch.as_slice());
                        scratch.clear();
                        item
                    };
                    observe::<VERIFY>(&item, &mut totals);
                } else if scratch.is_empty() {
                    observe::<VERIFY>(&buf[..end], &mut totals);
                } else {
                    scratch.extend_from_slice(&buf[..end]);
                    observe::<VERIFY>(&scratch, &mut totals);
                    scratch.clear();
                }
                reader.consume(end);
            } else {
                scratch.extend_from_slice(buf);
                let len = buf.len();
                reader.consume(len);
            }
        }
    }
    totals
}

// This control requires every record to fit in the current fill_buf result.
// It is only used with aligned inputs, alongside the ordinary scratch-capable
// loop on the same file and capacity. A short read that splits a record makes
// the experiment fail instead of silently dropping or changing that record.
#[inline(never)]
fn without_scratch<R: BufRead, const OWNED: bool, const VERIFY: bool>(
    mut reader: R,
) -> (usize, usize) {
    let mut totals = (0, 0);
    loop {
        let buf = reader.fill_buf().unwrap();
        if buf.is_empty() {
            break;
        }
        let end = find_line_end(buf).expect("aligned experiment encountered a partial record");
        if OWNED {
            let item = copy_frame(&buf[..end]);
            observe::<VERIFY>(&item, &mut totals);
        } else {
            observe::<VERIFY>(&buf[..end], &mut totals);
        }
        reader.consume(end);
    }
    totals
}

// The same complete-record assumption, using take_view to consume the record.
// Comparing this with without_scratch::<BytesReader<_>> separates the record
// operation from the parsing state maintained by next_end.
#[inline(never)]
fn without_scratch_take_view<R: Read, const OWNED: bool, const VERIFY: bool>(
    mut reader: BytesReader<R>,
) -> (usize, usize) {
    let mut totals = (0, 0);
    loop {
        let buf = reader.fill_buf().unwrap();
        if buf.is_empty() {
            break;
        }
        let end = find_line_end(buf).expect("aligned experiment encountered a partial record");
        let record = reader.take_view(..end, None);
        if OWNED {
            let item = copy_frame(record);
            observe::<VERIFY>(&item, &mut totals);
        } else {
            observe::<VERIFY>(record, &mut totals);
        }
    }
    totals
}

// A direct BytesMut buffer: advance after borrowed/copied frames, or split off
// owned frames. Remember the scanned prefix so partial lines are searched once.
#[inline(never)]
fn bytes_mut<R: Read, const MODE: usize, const VERIFY: bool>(
    mut source: R,
    capacity: usize,
) -> (usize, usize) {
    let mut buffer = BytesMut::with_capacity(capacity);
    let mut lines = Lines::default();
    let mut totals = (0, 0);
    loop {
        if let Some(end) = lines.line_end(&buffer) {
            match MODE {
                0 => {
                    observe::<VERIFY>(&buffer[..end], &mut totals);
                    buffer.advance(end);
                }
                1 => {
                    let item = copy_frame(&buffer[..end]);
                    observe::<VERIFY>(&item, &mut totals);
                    buffer.advance(end);
                }
                2 => observe::<VERIFY>(&buffer.split_to(end), &mut totals),
                _ => unreachable!(),
            }
        } else {
            // Match the initial half-capacity refill target. BytesMut handles
            // reclaiming the prefix removed by advance/split_to itself.
            buffer.reserve(capacity / 2);
            let filled = buffer.len();
            #[cfg(feature = "nightly")]
            let nread = {
                // Use the same read_buf interface as BytesReader.
                let mut dst = BorrowedBuf::from(buffer.spare_capacity_mut());
                source.read_buf(dst.unfilled()).unwrap();
                let nread = dst.len();
                // SAFETY: BorrowedBuf's filled prefix contains nread
                // initialized bytes following the existing input.
                unsafe { buffer.set_len(filled + nread) };
                nread
            };
            #[cfg(not(feature = "nightly"))]
            let nread = {
                // Read::read requires initialized bytes. This direct BytesMut
                // keeps only input in len(), so resize initializes the writable
                // tail on each refill; it has no separate initialization bound.
                buffer.resize(buffer.capacity(), 0);
                let dst = &mut buffer[filled..];
                let nread = source.read(dst).unwrap();
                assert!(
                    nread <= dst.len(),
                    "reader returned more bytes than requested"
                );
                buffer.truncate(filled + nread);
                nread
            };
            if nread == 0 {
                assert!(buffer.is_empty(), "incomplete line");
                break;
            }
        }
    }
    totals
}

fn run<R: Read, const VERIFY: bool>(source: R, mode: usize, capacity: usize) -> (usize, usize) {
    match mode {
        0 => buffered::<_, 0, VERIFY>(source, capacity),
        1 => buffered::<_, 1, VERIFY>(source, capacity),
        2 => buffered::<_, 2, VERIFY>(source, capacity),
        3 => standard::<_, 0, true, VERIFY>(source, capacity),
        4 => standard::<_, 1, true, VERIFY>(source, capacity),
        5 => standard::<_, 2, true, VERIFY>(source, capacity),
        6 => bytes_mut::<_, 0, VERIFY>(source, capacity),
        7 => bytes_mut::<_, 1, VERIFY>(source, capacity),
        8 => bytes_mut::<_, 2, VERIFY>(source, capacity),
        9 => accumulating::<_, false, VERIFY>(source, capacity),
        10 => accumulating::<_, true, VERIFY>(source, capacity),
        11 => standard::<_, 1, false, VERIFY>(source, capacity),
        12 => standard::<_, 3, true, VERIFY>(source, capacity),
        13 => without_scratch::<_, false, VERIFY>(BufReader::with_capacity(capacity, source)),
        14 => without_scratch::<_, true, VERIFY>(BufReader::with_capacity(capacity, source)),
        15 => without_scratch::<_, false, VERIFY>(BytesReader::with_capacity(capacity, source)),
        16 => without_scratch::<_, true, VERIFY>(BytesReader::with_capacity(capacity, source)),
        17 => without_scratch_take_view::<_, false, VERIFY>(BytesReader::with_capacity(
            capacity, source,
        )),
        18 => without_scratch_take_view::<_, true, VERIFY>(BytesReader::with_capacity(
            capacity, source,
        )),
        _ => unreachable!(),
    }
}

fn sorted_median(values: &[f64]) -> Option<f64> {
    let middle = values.len() / 2;
    let upper = *values.get(middle)?;
    Some(if values.len().is_multiple_of(2) {
        (values[middle - 1] + upper) / 2.0
    } else {
        upper
    })
}

#[derive(Default)]
struct Results {
    comparison: Vec<table::Row<9>>,
    limited: Vec<table::Row<9>>,
}

fn compare<R: Read + Seek>(
    source: &mut R,
    kind: &str,
    expected: (usize, usize),
    verify_only: bool,
    capacity: usize,
    show_round_times: bool,
    modes: &[usize],
) -> Results {
    // Verify every byte before timing; timed runs only present each frame to
    // black_box.
    for &mode in modes {
        source.rewind().unwrap();
        assert_eq!(run::<_, true>(&mut *source, mode, capacity), expected);
    }
    if verify_only {
        return Results::default();
    }
    let mut times: [Vec<f64>; METHODS.len()] = Default::default();
    let samples = modes.len();
    // Two warm-ups; rotate methods so no method always runs first.
    for sample in 0..samples + 2 {
        for order in 0..modes.len() {
            let mode = modes[(sample + order) % modes.len()];
            let mut elapsed = 0.0;
            for _ in 0..ROUNDS {
                source.rewind().unwrap();
                let start = Instant::now();
                // Include reader construction, reading, decoding and
                // destruction.
                // Each owned result is dropped before decoding the next Item.
                let totals = run::<_, false>(&mut *source, mode, capacity);
                elapsed += start.elapsed().as_secs_f64();
                assert_eq!(totals, expected);
            }
            let elapsed = elapsed * 1e9 / (expected.0 * ROUNDS) as f64;
            if sample >= 2 {
                times[mode].push(elapsed);
                if show_round_times {
                    println!("sample,{kind},{mode},{},{elapsed:.6}", sample - 2);
                }
            }
        }
    }
    for values in &mut times {
        values.sort_by(f64::total_cmp);
    }
    // Compare identical output types; Vec and BytesMut have different
    // ownership and allocation costs even though both own their bytes.
    // Restricted controls cannot handle split records and do not participate
    // in the ordinary comparisons, even when the aligned input lets them run.
    let borrowed_best = [1, 3, 6]
        .into_iter()
        .filter_map(|mode| sorted_median(&times[mode]))
        .fold(f64::INFINITY, f64::min);
    let vec_best = [2, 4, 5, 7, 9, 10, 11]
        .into_iter()
        .filter_map(|mode| sorted_median(&times[mode]))
        .fold(f64::INFINITY, f64::min);
    let bytes_best = [0, 8, 12]
        .into_iter()
        .filter_map(|mode| sorted_median(&times[mode]))
        .fold(f64::INFINITY, f64::min);
    let mut results = Results::default();
    for mode in DISPLAY_ORDER {
        let (reader, method, spans_reads) = METHODS[mode];
        let values = &times[mode];
        let Some(median) = sorted_median(values) else {
            continue;
        };
        let (item, best) = match mode {
            1 | 3 | 6 | 13 | 15 | 17 => ("&[u8]", borrowed_best),
            0 | 8 | 12 => ("BytesMut", bytes_best),
            _ => ("Vec<u8>", vec_best),
        };
        let limited = LIMITED_MODES.contains(&mode);
        let row = table::Row {
            fastest: !limited && median == best,
            subgroup: None,
            cells: [
                kind.to_owned(),
                reader.to_owned(),
                item.to_owned(),
                method.to_owned(),
                spans_reads.to_owned(),
                format!("{median:.2}"),
                format!("{:.2}", values[0]),
                format!("{:.2}", values[samples - 1]),
                format!(
                    "{:.2}",
                    expected.1 as f64 / expected.0 as f64 * 1e9 / median / (1024.0 * 1024.0)
                ),
            ],
        };
        if limited {
            results.limited.push(row);
        } else {
            results.comparison.push(row);
        }
    }
    results
}

fn print_results(results: &Results) {
    print_rows(&results.comparison, false);
    if !results.limited.is_empty() {
        table::text(
            "\nLimited tests compare BytesReader's take_view and both readers' fill_buf + slice + consume when each record arrives in one read. They also compare copying the slice into a Vec. These loops cannot join split records and are excluded from rankings.",
        );
        print_rows(&results.limited, true);
    }
}

fn print_rows(rows: &[table::Row<9>], limited: bool) {
    for group in rows.chunk_by(|a, b| a.cells[0] == b.cells[0]) {
        let mut notes: Vec<&str> = Vec::new();
        let mut compact: Vec<_> = group
            .iter()
            .map(|row| {
                let [
                    _,
                    reader,
                    item,
                    call,
                    spans_reads,
                    median,
                    min,
                    max,
                    throughput,
                ] = &row.cells;
                let note = if let Some(index) = notes.iter().position(|text| *text == spans_reads) {
                    index + 1
                } else {
                    notes.push(spans_reads);
                    notes.len()
                };
                table::Row {
                    cells: [
                        item.clone(),
                        format!("[{note}] {reader}: {call}"),
                        median.clone(),
                        min.clone(),
                        max.clone(),
                        throughput.clone(),
                    ],
                    fastest: row.fastest,
                    subgroup: Some(note),
                }
            })
            .collect();
        // Compare matching partial-record handling within each result type.
        // Stable sorting keeps the reader order within each footnote group.
        for item in compact.chunk_by_mut(|a, b| a.cells[0] == b.cells[0]) {
            item.sort_by_key(|row| row.subgroup);
        }
        table::print(
            &format!(
                "{}{} | time: ns/record",
                if limited {
                    "Limited test (no ranking) | "
                } else {
                    ""
                },
                group[0].cells[0]
            ),
            ["Item", "call", "median", "min", "max", "MiB/s"],
            &compact,
            2,
            1,
        );
        table::text("When a record spans reads:");
        for (index, note) in notes.iter().enumerate() {
            table::text(&format!("[{}] {note}", index + 1));
        }
    }
}

fn print_notes(samples: usize) {
    table::text("Item = result type; buf = current input slice; end = just after the newline.");
    table::text(
        "Lower time and higher MiB/s are better. Green compares the same result type and source, excluding limited tests.",
    );
    table::text(&format!(
        "{samples} timed rounds, {ROUNDS} full passes per round, after two warm-ups; order rotates. \
         Includes reading, newline search and record handling; verifies contents before timing."
    ));
}

fn main() -> io::Result<()> {
    table::text(&format!(
        "Read API for BytesReader and direct BytesMut: {}",
        if cfg!(feature = "nightly") {
            "Read::read_buf (nightly feature enabled)"
        } else {
            "Read::read (initialized destinations)"
        }
    ));
    let mut summary = true;
    let mut verify_only = false;
    let mut aligned_records = false;
    let mut show_round_times = false;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--summary=yes" => summary = true,
            "--summary=no" => summary = false,
            "--test" => verify_only = true,
            "--aligned-records" => aligned_records = true,
            "--show-round-times" => show_round_times = true,
            // Cargo supplies this argument when running a benchmark target.
            "--bench" => {}
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "unknown argument {arg:?}; expected --summary=yes, --summary=no, --aligned-records, --show-round-times, or --test"
                    ),
                ));
            }
        }
    }
    if show_round_times && summary && !aligned_records {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--show-round-times requires --summary=no or --aligned-records",
        ));
    }
    if show_round_times {
        println!("Per-round timings below show variation behind the final median (ns/record).");
        println!("# case,payload_bytes,record_bytes,buffer_bytes,record_count,file_bytes");
        println!("# sample,payload=N,method_number,round_number,ns_per_record");
        println!("# method,number,reader,call,handling_when_record_spans_reads");
        for mode in if aligned_records {
            &ALIGNED_MODES[..]
        } else {
            &DEFAULT_MODES[..]
        } {
            let (reader, call, spans_reads) = METHODS[*mode];
            println!("method,{mode},{reader},\"{call}\",\"{spans_reads}\"");
        }
    }
    if !summary || aligned_records {
        return individual_files(verify_only, aligned_records, show_round_times);
    }
    // Exercise repeated accumulation and truncation separately from timing;
    // normal workloads also verify every complete Item before each benchmark.
    let mut check = BytesReader::with_capacity(2, io::Cursor::new(b"abc\nxy\nz"));
    let mut partial = Vec::new();
    assert_eq!(
        next_accumulated::<_, false>(&mut check, &mut partial)?.as_deref(),
        Some(&b"abc\n"[..])
    );
    assert_eq!(
        next_accumulated::<_, false>(&mut check, &mut partial)?.as_deref(),
        Some(&b"xy\n"[..])
    );
    assert_eq!(
        next_accumulated::<_, false>(&mut check, &mut partial)
            .unwrap_err()
            .kind(),
        io::ErrorKind::UnexpectedEof
    );

    drop(check);
    // Include a timestamp so an interrupted run's file cannot collide with a
    // later process that receives the same PID.
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "bytesreader-read-bench-{}-{stamp}",
        std::process::id()
    ));
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)?;
    let workloads: [(&str, Vec<usize>); 4] = [
        ("small-variable", (20..=500).step_by(20).collect()),
        ("medium-variable", (520..=1000).step_by(20).collect()),
        ("large-variable", (1020..=1500).step_by(20).collect()),
        // Avoid exact multiples of the read buffer so boundaries move across
        // reads. These Items exercise repeated refills and buffer growth.
        (
            "oversized-variable",
            vec![
                (16 << 10) + 3,
                (64 << 10) + 5,
                (100 << 10) + 7,
                (256 << 10) + 11,
                (1 << 20) + 13,
            ],
        ),
    ];
    for (label, sizes) in workloads {
        // All lengths include LF. Payload bytes exclude LF and vary by
        // position so verification can detect incorrect frame contents.
        let mut data = Vec::new();
        let mut frames = 0;
        while data.len() < 4 * 1024 * 1024 || frames < 64 {
            let size = sizes[frames % sizes.len()];
            let start = data.len();
            data.extend((start..start + size).map(|i| (i % 245 + 11) as u8));
            *data.last_mut().unwrap() = b'\n';
            frames += 1;
        }
        let expected = (frames, data.len());
        let mut results = compare(
            &mut io::Cursor::new(&data),
            "memory (Cursor)",
            expected,
            verify_only,
            CAPACITY,
            false,
            &DEFAULT_MODES,
        );
        file.rewind()?;
        file.set_len(0)?;
        file.write_all(&data)?;
        // Recently written files measure cached I/O, not storage-device
        // latency.
        let file_results = compare(
            &mut file,
            "file (OS cache)",
            expected,
            verify_only,
            CAPACITY,
            false,
            &DEFAULT_MODES,
        );
        results.comparison.extend(file_results.comparison);
        results.limited.extend(file_results.limited);
        if verify_only {
            println!(
                "{label}: verified {} methods on memory and file-cached \
                 sources ({frames} Items, {} bytes each pass)",
                DEFAULT_MODES.len(),
                data.len(),
            );
            continue;
        }
        table::text(&format!(
            "\n{label}: {} sizes mixed, {}..={} B including newline; initial buffer {CAPACITY} B",
            sizes.len(),
            sizes[0],
            sizes[sizes.len() - 1],
        ));
        print_results(&results);
        print_notes(SAMPLES);
    }
    drop(file);
    std::fs::remove_file(path)
}

// Per-size steady-state comparisons use a larger buffer for larger records.
// Sizes below exclude the final LF.
fn individual_files(
    verify_only: bool,
    aligned_records: bool,
    show_round_times: bool,
) -> io::Result<()> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "bytesreader-individual-{}-{stamp}",
        std::process::id()
    ));
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)?;
    for payload in
        (0..=1500)
            .step_by(20)
            .chain([4096, 8192, 16 * 1024, 32 * 1024, 64 * 1024, 100 * 1024])
    {
        let capacity = if payload < 4096 { CAPACITY } else { 1 << 20 };
        let record_len = payload + 1;
        let records_per_buffer = capacity / record_len;
        let capacity = if aligned_records {
            records_per_buffer * record_len
        } else {
            capacity
        };
        // Empty records still contain LF. One MiB gives more than a million
        // Items; other sizes use at least 4 MiB and 64 complete Items.
        let min_bytes: usize = if payload == 0 { 1 << 20 } else { 4 << 20 };
        let frames = min_bytes.div_ceil(record_len).max(64);
        let frames = if aligned_records {
            frames.div_ceil(records_per_buffer) * records_per_buffer
        } else {
            frames
        };
        let mut data = Vec::with_capacity(frames * record_len);
        for _ in 0..frames {
            let start = data.len();
            data.extend((start..start + payload).map(|i| (i % 245 + 11) as u8));
            data.push(b'\n');
        }
        file.rewind()?;
        file.set_len(0)?;
        file.write_all(&data)?;
        let kind = format!("payload={payload}");
        let modes: &[usize] = if aligned_records {
            &ALIGNED_MODES
        } else {
            &DEFAULT_MODES
        };
        if show_round_times {
            println!(
                "case,{payload},{record_len},{capacity},{frames},{}",
                data.len()
            );
        }
        let mut results = compare(
            &mut file,
            &kind,
            (frames, data.len()),
            verify_only,
            capacity,
            show_round_times,
            modes,
        );
        if verify_only {
            println!("verified {payload} B payload: {} methods", modes.len());
        } else {
            table::text(&format!(
                "\n{payload} B payload + 1 B newline; buffer {capacity} B; {frames} records"
            ));
            for row in results.comparison.iter_mut().chain(&mut results.limited) {
                row.cells[0] = "file (OS cache)".to_owned();
            }
            print_results(&results);
            print_notes(modes.len());
        }
    }
    drop(file);
    std::fs::remove_file(path)
}
