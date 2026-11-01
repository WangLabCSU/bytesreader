# Benchmarks

Run from the repository root:

```sh
cargo bench --bench read
cargo bench --bench write
```

To benchmark the nightly read path:

```sh
cargo +nightly bench --bench read --features nightly
```

By default, `FramedRead` and direct `BytesMut` use `Read::read` with initialized
destinations. `FramedRead` tracks initialized spare space; direct `BytesMut`
initializes its tail on each refill. With `nightly`, both use `Read::read_buf`.

Read compares `FramedRead`, `BufReader`, and direct `BytesMut` buffering.
Write compares `FramedWrite` and `BufWriter`.

| Workload              | Item sizes             |
| --------------------- | ---------------------- |
| Small                 | 20–500 B, step 20 B    |
| Medium                | 520–1000 B, step 20 B  |
| Large                 | 1020–1500 B, step 20 B |
| Oversized (read only) | See sizes below        |

Oversized Items repeat 16 KiB + 3 B, 64 KiB + 5 B, 100 KiB + 7 B,
256 KiB + 11 B, and 1 MiB + 13 B.

Read sizes include the final newline. Write sizes are the bytes written per
Item. Initial buffer capacity is 8 KiB.

Each workload mixes its listed sizes in one dataset. Its table reports the
whole mixture, not individual results for each size. The oversized workload
exercises Items larger than the initial buffer, including buffer growth.

To check read results without timing, append `-- --test` to either read command.

Reading the tables:

- `call` shows the operation being measured.
- `median ns/Item`, `min`, and `max` are nanoseconds per Item; lower is faster.
  Green marks the lowest median within each comparison group.
- Read tables also show `median MiB/s`; higher throughput is better.
- Read rows are grouped by source and result type: borrowed slice, `Vec<u8>`,
  or `BytesMut`. Write rows are grouped by sink.
- `Lines` keeps partial lines in the reader; `AccumulatingLines` stores them
  in its own `Vec`. `false` transfers that Vec; `true` copies it and reuses it.

File reads use cached data. File writes include flushing but not `sync_all()`.

## Profiling reads with hotpath

Profiling is opt-in. `hotpath` records function timings and file I/O;
`hotpath-alloc` also records allocation counts and bytes. The example uses
real sequential file reads from the page cache, including framing, decoding,
Item consumption and destruction. It verifies every byte and warms the file
before the measured passes. The `profiling` build profile keeps release
optimization and debug symbols.

```sh
cargo run --profile profiling --example profile_read --features hotpath -- bytes 180 8192 100
cargo run --profile profiling --example profile_read --features hotpath-alloc -- bytes 180 8192 100
cargo run --profile profiling --example profile_read --features hotpath -- owned 180 8192 100
cargo run --profile profiling --example profile_read --features hotpath -- view 102400 1048576 100
```

Arguments are decoding mode (`bytes`, `view`, or `owned`), payload bytes,
buffer capacity in bytes, and complete file passes. Each Item includes one
additional newline. Defaults are `owned 180 8192 100`. The buffer must hold
multiple Items. Input generation is outside the measured passes, and the
temporary input file is removed on exit.

Instrumentation covers the complete file and the individual reading stages:

- `FramedRead::decode_bytes`, `decode_view`, `decode_owned` and `frame_ready`.
- The example's `Lines::frame` scanner and `Lines::decode_owned` allocation
  and copy. Its bytes and view decoders only return their input.
- Buffer extraction (`take_bytes`, `take_view`) and byte splitting (`split_bytes`).
- Buffer creation, refill and making room for more input.

Accessors and individual consumption, initialization and append helpers
are not instrumented separately. A report only contains operations that ran;
for example, `take_view` is absent when bytes Items are always split off.
The example shows all recorded functions instead of limiting the table to the
top entries. To distinguish work inside your own codec, annotate its costly
framing or decoding methods as demonstrated by `Lines`.

Durations include nested calls: `frame_ready` includes `Lines::frame` and any
refill; `decode_owned` includes framing, extraction and `Lines::decode_owned`.
`take_bytes` includes `split_bytes` when the selection can be split off, or
`take_view` and allocation and copying when it overlaps retained input.
Do not add parent and child durations together. Allocation rows are exclusive
by default. The owned example copies each full Item into a fresh `Vec`, now
attributed to `Lines::decode_owned`; other `OwnedDecoder` implementations need
not do so. Item destruction occurs after the reader's decode method returns
and is included in the outer `read_file` measurement.

Per-Item instrumentation can dominate small operations and affect inlining.
To time a random 1% of function calls while still recording all call counts:

```sh
HOTPATH_FUNCTIONS_TIME_SAMPLING_RATE=0.01 \
  cargo run --profile profiling --example profile_read --features hotpath -- bytes 180 8192 100
```

Set the rate to `0` for counts without function timings. Sampling reduces clock
reads, but does not eliminate instrumentation overhead or turn these timings
into CPU samples. See hotpath's [overhead documentation](https://hotpath.rs/profiling_overhead).

Instrumented timings include profiler overhead and are not benchmark results.
Validate any optimization with profiling disabled using the ordinary bench
commands above. No Criterion conversion is needed for these custom benches.

To save a static JSON report without starting the local metrics server:

```sh
HOTPATH_METRICS_SERVER_OFF=1 HOTPATH_OUTPUT_FORMAT=json-pretty \
  HOTPATH_OUTPUT_PATH=target/read-profile.json \
  cargo run --profile profiling --example profile_read --features hotpath-alloc -- owned 180 8192 100
```

Adding `nightly` uses the native `File::read_buf` path. This skips the I/O
wrapper because it does not forward native `read_buf`; function and allocation
reports still work:

```sh
cargo +nightly run --profile profiling --example profile_read --features nightly,hotpath-alloc -- owned 180 8192 100
```

Static reports need no extra tools. For an optional live dashboard, install
`cargo install hotpath --features=tui`, run `hotpath console`, then run one of
the profiling commands in a second terminal without
`HOTPATH_METRICS_SERVER_OFF=1`. Short runs may finish before the dashboard
refreshes; increase the passes to observe them live.

See the [hotpath setup guide](https://hotpath.rs/agent/setup) and
[profiling modes](https://hotpath.rs/profiling_modes). To make its documentation
discoverable in future agent sessions, optionally install its agent skill:
`npx skills add hotpath-rs/hotpath --skill hotpath-rs`.
