# bytesreader

[<img alt="github" src="https://img.shields.io/badge/github-WangLabCSU/bytesreader-8da0cb?style=for-the-badge&labelColor=555555&logo=github" height="20">](https://github.com/WangLabCSU/bytesreader)
[<img alt="crates.io" src="https://img.shields.io/crates/v/bytesreader.svg?style=for-the-badge&color=fc8d62&logo=rust" height="20">](https://crates.io/crates/bytesreader)
[<img alt="docs.rs" src="https://img.shields.io/badge/docs.rs-bytesreader-66c2a5?style=for-the-badge&labelColor=555555&logo=docs.rs" height="20">](https://docs.rs/bytesreader)
[<img alt="build status" src="https://img.shields.io/github/actions/workflow/status/WangLabCSU/bytesreader/ci.yml?branch=main&style=for-the-badge" height="20">](https://github.com/WangLabCSU/bytesreader/actions?query=branch%3Amain)

**bytesreader** is a growable buffered reader for records, including those
larger than its input buffer. Keep data in the buffer while reading more,
then borrow a slice or take ownership of it as a `BytesMut`.

The crate works on stable Rust. The optional `nightly` feature uses
`Read::read_buf` to fill the buffer and requires a nightly toolchain.

## Performance

For owned records smaller than roughly 1 KiB, `BufReader` with `to_vec()` was
often faster in these tests. For larger records, `BytesReader::take_bytes`
can be faster: when you consume the selected bytes, it can return them as a
`BytesMut` without copying.

The benchmarks below read newline-delimited records from cached files. For
each selected payload size, the tables show the range of median times across
three runs. An asterisk marks the lowest median of those three run medians;
close results may be measurement noise. Borrowed and owned results are
compared separately. The `BufReader` paths use `fill_buf`, slice the record,
and call `consume`.

When a record spans reads, the `BufReader` examples collect its parts in a
temporary `Vec`. One copies the completed record and reuses the temporary
buffer; the other returns it with `mem::take`. When a record fits in one
read, both copy it into a `Vec`.

Borrowing a slice:

| Payload | `BytesReader::take_view` | `BufReader`: `fill_buf` + slice + `consume` |
| ------- | ------------------------ | ------------------------------------------- |
| 100 B   | 10.98–11.34 ns           | 10.91–11.22 ns *                            |
| 400 B   | 31.95–32.58 ns           | 29.96–31.08 ns *                            |
| 800 B   | 58.24–59.02 ns           | 53.88–54.66 ns *                            |
| 1000 B  | 70.66–71.56 ns           | 67.11–68.19 ns *                            |
| 1020 B  | 71.77–72.18 ns           | 63.86–64.60 ns *                            |
| 1040 B  | 76.46–79.05 ns           | 70.71–72.96 ns *                            |
| 1180 B  | 88.02–89.16 ns           | 77.86–79.43 ns *                            |
| 1500 B  | 106.20–108.98 ns         | 96.67–99.80 ns *                            |
| 4 KiB   | 235.26–243.41 ns *       | 236.51–242.68 ns                            |
| 100 KiB | 5.87–6.03 µs *           | 5.86–6.13 µs                                |

Owned records, grouped by payload so you can compare `take_bytes` and
`to_vec()` directly:

| Payload | Method                                          | Time per record    |
| ------- | ----------------------------------------------- | ------------------ |
| 100 B   | `BytesReader::take_bytes` → `BytesMut`          | 17.80–18.17 ns     |
|         | `BufReader` + `to_vec` → `Vec<u8>`              | 14.13–14.25 ns *   |
|         | `BufReader` + `mem::take` → `Vec<u8>`           | 14.71–15.05 ns     |
|         | `BytesReader::take_view` + `to_vec` → `Vec<u8>` | 14.62–14.85 ns     |
| 400 B   | `BytesReader::take_bytes` → `BytesMut`          | 37.67–39.46 ns     |
|         | `BufReader` + `to_vec` → `Vec<u8>`              | 35.04–36.02 ns *   |
|         | `BufReader` + `mem::take` → `Vec<u8>`           | 36.67–37.49 ns     |
|         | `BytesReader::take_view` + `to_vec` → `Vec<u8>` | 36.12–37.38 ns     |
| 800 B   | `BytesReader::take_bytes` → `BytesMut`          | 64.00–65.40 ns     |
|         | `BufReader` + `to_vec` → `Vec<u8>`              | 62.94–64.22 ns *   |
|         | `BufReader` + `mem::take` → `Vec<u8>`           | 68.43–69.02 ns     |
|         | `BytesReader::take_view` + `to_vec` → `Vec<u8>` | 67.84–68.56 ns     |
| 820 B   | `BytesReader::take_bytes` → `BytesMut`          | 66.19–69.51 ns     |
|         | `BufReader` + `to_vec` → `Vec<u8>`              | 64.41–66.82 ns *   |
|         | `BufReader` + `mem::take` → `Vec<u8>`           | 70.70–73.09 ns     |
|         | `BytesReader::take_view` + `to_vec` → `Vec<u8>` | 69.64–73.20 ns     |
| 840 B   | `BytesReader::take_bytes` → `BytesMut`          | 67.81–68.76 ns     |
|         | `BufReader` + `to_vec` → `Vec<u8>`              | 65.20–67.03 ns *   |
|         | `BufReader` + `mem::take` → `Vec<u8>`           | 71.95–73.78 ns     |
|         | `BytesReader::take_view` + `to_vec` → `Vec<u8>` | 70.75–72.46 ns     |
| 860 B   | `BytesReader::take_bytes` → `BytesMut`          | 68.68–69.26 ns     |
|         | `BufReader` + `to_vec` → `Vec<u8>`              | 67.92–68.47 ns *   |
|         | `BufReader` + `mem::take` → `Vec<u8>`           | 74.53–75.10 ns     |
|         | `BytesReader::take_view` + `to_vec` → `Vec<u8>` | 71.70–73.66 ns     |
| 880 B   | `BytesReader::take_bytes` → `BytesMut`          | 68.60–69.84 ns     |
|         | `BufReader` + `to_vec` → `Vec<u8>`              | 68.91–71.52 ns *   |
|         | `BufReader` + `mem::take` → `Vec<u8>`           | 75.58–78.82 ns     |
|         | `BytesReader::take_view` + `to_vec` → `Vec<u8>` | 71.73–73.40 ns     |
| 900 B   | `BytesReader::take_bytes` → `BytesMut`          | 69.73–71.65 ns *   |
|         | `BufReader` + `to_vec` → `Vec<u8>`              | 70.41–73.52 ns     |
|         | `BufReader` + `mem::take` → `Vec<u8>`           | 77.01–77.52 ns     |
|         | `BytesReader::take_view` + `to_vec` → `Vec<u8>` | 71.96–74.42 ns     |
| 920 B   | `BytesReader::take_bytes` → `BytesMut`          | 71.94–74.81 ns     |
|         | `BufReader` + `to_vec` → `Vec<u8>`              | 71.41–73.04 ns *   |
|         | `BufReader` + `mem::take` → `Vec<u8>`           | 78.01–78.91 ns     |
|         | `BytesReader::take_view` + `to_vec` → `Vec<u8>` | 77.08–78.54 ns     |
| 940 B   | `BytesReader::take_bytes` → `BytesMut`          | 73.54–75.69 ns     |
|         | `BufReader` + `to_vec` → `Vec<u8>`              | 71.91–75.59 ns *   |
|         | `BufReader` + `mem::take` → `Vec<u8>`           | 79.89–84.23 ns     |
|         | `BytesReader::take_view` + `to_vec` → `Vec<u8>` | 77.20–80.48 ns     |
| 960 B   | `BytesReader::take_bytes` → `BytesMut`          | 74.89–75.36 ns     |
|         | `BufReader` + `to_vec` → `Vec<u8>`              | 72.34–74.67 ns *   |
|         | `BufReader` + `mem::take` → `Vec<u8>`           | 80.05–81.80 ns     |
|         | `BytesReader::take_view` + `to_vec` → `Vec<u8>` | 79.68–79.93 ns     |
| 980 B   | `BytesReader::take_bytes` → `BytesMut`          | 74.95–77.26 ns     |
|         | `BufReader` + `to_vec` → `Vec<u8>`              | 74.44–76.02 ns *   |
|         | `BufReader` + `mem::take` → `Vec<u8>`           | 81.02–82.84 ns     |
|         | `BytesReader::take_view` + `to_vec` → `Vec<u8>` | 80.87–83.20 ns     |
| 1000 B  | `BytesReader::take_bytes` → `BytesMut`          | 76.12–77.07 ns *   |
|         | `BufReader` + `to_vec` → `Vec<u8>`              | 76.27–78.60 ns     |
|         | `BufReader` + `mem::take` → `Vec<u8>`           | 81.74–84.23 ns     |
|         | `BytesReader::take_view` + `to_vec` → `Vec<u8>` | 79.19–81.74 ns     |
| 1020 B  | `BytesReader::take_bytes` → `BytesMut`          | 76.84–77.70 ns     |
|         | `BufReader` + `to_vec` → `Vec<u8>`              | 72.00–73.52 ns *   |
|         | `BufReader` + `mem::take` → `Vec<u8>`           | 78.37–82.67 ns     |
|         | `BytesReader::take_view` + `to_vec` → `Vec<u8>` | 79.77–82.21 ns     |
| 1040 B  | `BytesReader::take_bytes` → `BytesMut`          | 81.23–86.96 ns *   |
|         | `BufReader` + `to_vec` → `Vec<u8>`              | 97.89–100.37 ns    |
|         | `BufReader` + `mem::take` → `Vec<u8>`           | 94.63–100.50 ns    |
|         | `BytesReader::take_view` + `to_vec` → `Vec<u8>` | 99.36–108.17 ns    |
| 1060 B  | `BytesReader::take_bytes` → `BytesMut`          | 82.03–85.06 ns *   |
|         | `BufReader` + `to_vec` → `Vec<u8>`              | 88.55–91.93 ns     |
|         | `BufReader` + `mem::take` → `Vec<u8>`           | 93.90–96.21 ns     |
|         | `BytesReader::take_view` + `to_vec` → `Vec<u8>` | 97.19–103.57 ns    |
| 1080 B  | `BytesReader::take_bytes` → `BytesMut`          | 84.77–85.59 ns *   |
|         | `BufReader` + `to_vec` → `Vec<u8>`              | 94.49–97.62 ns     |
|         | `BufReader` + `mem::take` → `Vec<u8>`           | 99.62–103.31 ns    |
|         | `BytesReader::take_view` + `to_vec` → `Vec<u8>` | 101.51–103.45 ns   |
| 1100 B  | `BytesReader::take_bytes` → `BytesMut`          | 84.48–86.56 ns *   |
|         | `BufReader` + `to_vec` → `Vec<u8>`              | 100.78–106.92 ns   |
|         | `BufReader` + `mem::take` → `Vec<u8>`           | 99.58–103.22 ns    |
|         | `BytesReader::take_view` + `to_vec` → `Vec<u8>` | 101.87–102.78 ns   |
| 1120 B  | `BytesReader::take_bytes` → `BytesMut`          | 84.24–89.40 ns *   |
|         | `BufReader` + `to_vec` → `Vec<u8>`              | 95.43–97.90 ns     |
|         | `BufReader` + `mem::take` → `Vec<u8>`           | 97.66–103.66 ns    |
|         | `BytesReader::take_view` + `to_vec` → `Vec<u8>` | 100.09–103.24 ns   |
| 1140 B  | `BytesReader::take_bytes` → `BytesMut`          | 88.10–93.16 ns *   |
|         | `BufReader` + `to_vec` → `Vec<u8>`              | 99.13–101.29 ns    |
|         | `BufReader` + `mem::take` → `Vec<u8>`           | 104.86–109.32 ns   |
|         | `BytesReader::take_view` + `to_vec` → `Vec<u8>` | 104.55–108.10 ns   |
| 1160 B  | `BytesReader::take_bytes` → `BytesMut`          | 86.46–88.65 ns *   |
|         | `BufReader` + `to_vec` → `Vec<u8>`              | 99.21–105.48 ns    |
|         | `BufReader` + `mem::take` → `Vec<u8>`           | 103.14–121.17 ns   |
|         | `BytesReader::take_view` + `to_vec` → `Vec<u8>` | 107.48–123.13 ns   |
| 1180 B  | `BytesReader::take_bytes` → `BytesMut`          | 91.45–94.47 ns *   |
|         | `BufReader` + `to_vec` → `Vec<u8>`              | 100.56–103.16 ns   |
|         | `BufReader` + `mem::take` → `Vec<u8>`           | 106.32–107.18 ns   |
|         | `BytesReader::take_view` + `to_vec` → `Vec<u8>` | 112.71–113.55 ns   |
| 1500 B  | `BytesReader::take_bytes` → `BytesMut`          | 112.59–114.08 ns * |
|         | `BufReader` + `to_vec` → `Vec<u8>`              | 122.79–124.84 ns   |
|         | `BufReader` + `mem::take` → `Vec<u8>`           | 126.40–133.94 ns   |
|         | `BytesReader::take_view` + `to_vec` → `Vec<u8>` | 130.24–133.55 ns   |
| 4 KiB   | `BytesReader::take_bytes` → `BytesMut`          | 236.69–254.65 ns * |
|         | `BufReader` + `to_vec` → `Vec<u8>`              | 267.99–270.24 ns   |
|         | `BufReader` + `mem::take` → `Vec<u8>`           | 269.62–285.83 ns   |
|         | `BytesReader::take_view` + `to_vec` → `Vec<u8>` | 268.18–272.11 ns   |
| 100 KiB | `BytesReader::take_bytes` → `BytesMut`          | 6.00–6.20 µs *     |
|         | `BufReader` + `to_vec` → `Vec<u8>`              | 7.55–7.66 µs       |
|         | `BufReader` + `mem::take` → `Vec<u8>`           | 7.22–8.18 µs       |
|         | `BytesReader::take_view` + `to_vec` → `Vec<u8>` | 7.61–7.71 µs       |

The tests used Rust 1.101.0-nightly on Linux under WSL2, with an Intel
i7-13700KF. Initial buffers were 8 KiB for payloads through 1500 B and 1 MiB
for larger payloads. Each record included a newline, and owned results were
dropped before reading the next record. Keeping results longer may change
performance. Each size was measured in three runs of 13 rounds, with four
passes per round. To run the per-size comparison on your machine:

```sh
cargo +nightly bench --bench reading --features nightly -- --summary=no
```

See [benchmarks](benches/README.md) for the other options.

## Reading example

Use `take_view(range, consume)` to borrow part of the buffer, or `take_bytes`
to take it as an owned `BytesMut`. Both let you choose how much input to
consume. To copy a view into a `Vec<u8>`, call `to_vec()` on it.

For example, given `<abc>tail`, `take_view(1..4, Some(5))` returns `abc` and
leaves `tail`. Pass `None` instead of `Some(n)` to consume through the end of
the selected range.

`BytesReader` also provides the stable methods of
[`std::io::BufReader`](https://doc.rust-lang.org/std/io/struct.BufReader.html),
including `Read` and `BufRead`.

The `LineReader` below reads lines, calling `read_more` when a line spans
reads. Its `read_view` method borrows a line, `read_owned` copies it into a
`Vec<u8>`, and `read_bytes` returns a `BytesMut` that can outlive the reader.
The newline is consumed but not returned, and the last line does not need one.

```rust
use std::io::{self, Read};

use bytes::BytesMut;
use bytesreader::BytesReader;

struct LineReader<R> {
    reader: BytesReader<R>,
}

impl<R: Read> LineReader<R> {
    fn with_capacity(capacity: usize, source: R) -> Self {
        Self { reader: BytesReader::with_capacity(capacity, source) }
    }

    fn line_bounds(&mut self) -> io::Result<Option<(usize, usize)>> {
        let mut scanned = 0;
        loop {
            if let Some(offset) = self.reader.buffer()[scanned..].iter().position(|&b| b == b'\n') {
                let end = scanned + offset;
                return Ok(Some((end, end + 1)));
            }
            scanned = self.reader.buffer().len();
            match self.reader.read_more() {
                Ok(0) => return Ok(self.reader.has_remaining().then_some((scanned, scanned))),
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
    }

    fn read_bytes(&mut self) -> io::Result<Option<BytesMut>> {
        let Some((end, consume)) = self.line_bounds()? else {
            return Ok(None);
        };
        Ok(Some(self.reader.take_bytes(..end, Some(consume))))
    }

    fn read_view(&mut self) -> io::Result<Option<&[u8]>> {
        let Some((end, consume)) = self.line_bounds()? else {
            return Ok(None);
        };
        Ok(Some(self.reader.take_view(..end, Some(consume))))
    }

    fn read_owned(&mut self) -> io::Result<Option<Vec<u8>>> {
        Ok(self.read_view()?.map(|record| record.to_vec()))
    }
}

let mut reader = LineReader::with_capacity(4, &b"alpha\nbeta\ngamma"[..]);
let bytes = reader.read_bytes()?.unwrap();
assert_eq!(reader.read_view()?, Some(&b"beta"[..]));
let owned = reader.read_owned()?.unwrap();
assert_eq!(reader.read_view()?, None);
drop(reader);
assert_eq!(&bytes[..], b"alpha");
assert_eq!(owned, b"gamma");
# for capacity in [0, 1, 8] {
#     let mut reader = LineReader::with_capacity(capacity, &b"\n\n\nx\ny\nz"[..]);
#     assert!(reader.read_bytes()?.unwrap().is_empty());
#     assert_eq!(reader.read_view()?, Some(&b""[..]));
#     assert_eq!(reader.read_owned()?, Some(Vec::new()));
#     assert_eq!(&reader.read_bytes()?.unwrap()[..], b"x");
#     assert_eq!(reader.read_view()?, Some(&b"y"[..]));
#     assert_eq!(reader.read_owned()?, Some(b"z".to_vec()));
#     assert_eq!(reader.read_bytes()?, None);
#     assert_eq!(reader.read_view()?, None);
#     assert_eq!(reader.read_owned()?, None);
# }
# Ok::<(), io::Error>(())
```
