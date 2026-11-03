# Benchmarks

## reading

Compare complete record reads with `BytesReader`, `std::io::BufReader`, and
`BytesMut`. Results are grouped by item type and show median, minimum, and
maximum times, along with throughput.

```sh
cargo bench --bench reading
```

Pass options after `--`, for example: `cargo bench --bench reading -- --summary=no`.

| Argument             | Effect                                   |
| -------------------- | ---------------------------------------- |
| `--summary=yes`      | Compare mixed sizes (default).           |
| `--summary=no`       | Compare each payload size separately.    |
| `--show-round-times` | Also print the time for each round.      |
| `--aligned-records`  | Compare records that fit in one read.    |
| `--test`             | Verify output without timing.            |

`--show-round-times` requires `--summary=no` or `--aligned-records`.
Individual payloads range from 0 to 1500 B in 20 B steps, then 4, 8, 16, 32,
64 and 100 KiB. Each record also includes a newline byte.

`--aligned-records` adds a table comparing `take_view` with `fill_buf`,
slicing, and `consume` when each record arrives in one read. It includes
borrowed and copied results. A short read can split a record and cause these
tests to fail.

## methods

Compare `read_exact`, `read_vectored`, `read_to_end` and `read_to_string` on
`BytesReader` and `std::io::BufReader`. The benchmark also includes a
`BytesReader` wrapper that uses the default `Read` methods. Results are in
milliseconds per file and MiB/s.

```sh
cargo bench --bench methods
```

Both benchmarks support the `nightly` feature. For example:
`cargo +nightly bench --bench reading --features nightly`.
