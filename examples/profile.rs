//! Profile complete sequential file reads with a buffer larger than each Item.
//!
//! Arguments: `bytes|view|owned`, payload bytes, capacity bytes, passes.

use std::fs::{File, OpenOptions};
use std::hint::black_box;
use std::io::{self, Read, Seek, Write};
use std::path::PathBuf;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use bytesreader::BytesReader;
use memchr::memchr;

#[cfg(feature = "hotpath-alloc")]
#[global_allocator]
static ALLOCATOR: hotpath::CountingAllocator = hotpath::CountingAllocator::new();

#[derive(Default)]
struct Lines {
    scanned: usize,
}

impl Lines {
    #[hotpath::measure(impl_type = "Lines")]
    fn find_end(&mut self, src: &[u8]) -> Option<usize> {
        if let Some(offset) = memchr(b'\n', &src[self.scanned..]) {
            let end = self.scanned + offset + 1;
            self.scanned = 0;
            Some(end)
        } else {
            self.scanned = src.len();
            None
        }
    }

    #[hotpath::measure(impl_type = "Lines")]
    fn next_end<R: Read>(&mut self, reader: &mut BytesReader<R>) -> io::Result<Option<usize>> {
        loop {
            if let Some(end) = self.find_end(reader.buffer()) {
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
}

#[hotpath::measure]
fn copy_frame(frame: &[u8]) -> Vec<u8> {
    frame.to_vec()
}

fn observe<const VERIFY: bool>(item: &[u8], totals: &mut (usize, usize)) {
    if VERIFY {
        assert_eq!(item.last(), Some(&b'\n'));
        for (offset, &byte) in item[..item.len() - 1].iter().enumerate() {
            assert_eq!(byte, ((totals.1 + offset) % 245 + 11) as u8);
        }
    }
    black_box(item);
    totals.0 += 1;
    totals.1 += item.len();
}

// Each invocation creates a reader, reads the whole file, and drops every
// result before requesting the next Item. Rewinding happens at the call site.
fn process_file<R: Read, const MODE: usize, const VERIFY: bool>(
    source: R,
    capacity: usize,
) -> io::Result<(usize, usize)> {
    let mut reader = BytesReader::with_capacity(capacity, source);
    let mut lines = Lines::default();
    let mut totals = (0, 0);
    while let Some(end) = lines.next_end(&mut reader)? {
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
    Ok(totals)
}

// The outer span includes decoding, consuming Items, and dropping them.
#[hotpath::measure]
fn read_file<const MODE: usize>(file: &mut File, capacity: usize) -> io::Result<(usize, usize)> {
    // The stable wrapper measures actual file reads. It does not implement
    // native read_buf, so the nightly path keeps File's own implementation.
    #[cfg(not(feature = "nightly"))]
    let source = hotpath::io!(file, label = "input-file");
    #[cfg(feature = "nightly")]
    let source = file;
    process_file::<_, MODE, false>(source, capacity)
}

struct Input {
    file: File,
    path: PathBuf,
    items: usize,
    bytes: usize,
}

impl Input {
    fn new(payload: usize) -> io::Result<Self> {
        let item_size = payload.checked_add(1).expect("Item size overflow");
        let items = (4usize << 20).div_ceil(item_size).max(256);
        let bytes = items.checked_mul(item_size).expect("input size overflow");
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "bytesreader-profile-read-{}-{stamp}",
            std::process::id()
        ));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)?;
        let mut input = Self {
            file,
            path,
            items,
            bytes,
        };
        // Keep only a small generation chunk in memory; timed reads use File.
        let mut chunk = [0u8; 64 << 10];
        for start in (0..bytes).step_by(chunk.len()) {
            let len = chunk.len().min(bytes - start);
            for (offset, byte) in chunk[..len].iter_mut().enumerate() {
                let position = start + offset;
                *byte = if position % item_size == payload {
                    b'\n'
                } else {
                    (position % 245 + 11) as u8
                };
            }
            input.file.write_all(&chunk[..len])?;
        }
        input.file.rewind()?;
        Ok(input)
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn profile<const MODE: usize>(payload: usize, capacity: usize, passes: usize) -> io::Result<()> {
    let mut input = Input::new(payload)?;
    let expected = (input.items, input.bytes);
    assert_eq!(
        process_file::<_, MODE, true>(&mut input.file, capacity)?,
        expected
    );
    for _ in 0..5 {
        input.file.rewind()?;
        assert_eq!(
            process_file::<_, MODE, false>(&mut input.file, capacity)?,
            expected
        );
    }

    // Start the reporting session after data generation and content verification.
    let _guard = hotpath::HotpathGuardBuilder::new("profile")
        .functions_limit(0)
        .build();
    let mut elapsed = std::time::Duration::ZERO;
    for _ in 0..passes {
        input.file.rewind()?;
        let start = Instant::now();
        assert_eq!(read_file::<MODE>(&mut input.file, capacity)?, expected);
        elapsed += start.elapsed();
    }
    eprintln!(
        "payload={payload} capacity={capacity} passes={passes} Items/pass={} bytes/pass={} ns/Item={:.2}{}",
        input.items,
        input.bytes,
        elapsed.as_nanos() as f64 / (input.items as f64 * passes as f64),
        if cfg!(feature = "hotpath") {
            " (instrumented; not a benchmark result)"
        } else {
            ""
        },
    );
    Ok(())
}

fn main() -> io::Result<()> {
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or_else(|| "owned".into());
    let payload = args
        .next()
        .map_or(Ok(180), |v| v.parse::<usize>())
        .expect("invalid payload");
    let capacity = args
        .next()
        .map_or(Ok(8192), |v| v.parse::<usize>())
        .expect("invalid capacity");
    let passes = args
        .next()
        .map_or(Ok(100), |v| v.parse::<usize>())
        .expect("invalid passes");
    assert!(passes > 0, "passes must be positive");
    assert!(
        payload < capacity / 2,
        "use a buffer that holds multiple Items"
    );
    assert!(
        args.next().is_none(),
        "expected: bytes|view|owned payload capacity passes"
    );
    match mode.as_str() {
        "bytes" => profile::<0>(payload, capacity, passes),
        "view" => profile::<1>(payload, capacity, passes),
        "owned" => profile::<2>(payload, capacity, passes),
        _ => panic!("expected mode: bytes, view, or owned"),
    }
}
