#![doc = include_str!("../README.md")]
#![deny(missing_docs)]
#![cfg_attr(
    feature = "nightly",
    feature(borrowed_buf_init, core_io_borrowed_buf, read_buf)
)]

#[cfg(feature = "nightly")]
use std::io::BorrowedCursor;
use std::io::{self, BufRead, IoSliceMut, Read, Seek, SeekFrom};
use std::ops::RangeBounds;
use std::{fmt, str};

use bytes::BytesMut;

mod buffer;

use self::buffer::BytesBuf;

// ESP-IDF has a smaller default to accommodate limited RAM.
const DEFAULT_BUF_SIZE: usize = cfg_select! {
    target_os = "espidf" => 1 << 9,
    _ => 8 << 10,
};

/// A growable buffered reader backed by [`BytesMut`].
///
/// `BytesReader<R>` reads ahead from a source so that small reads can use data
/// already in memory. This reduces calls to the source when reading from files
/// or network streams.
///
/// It provides all stable methods of [`BufReader`](io::BufReader), including
/// the [`Read`] and [`BufRead`] methods. Sources that implement [`Seek`] also
/// support seeking through the reader.
///
/// Call [`read_more`](Self::read_more) when you need more input without
/// consuming the data already in the buffer. It grows the buffer if needed,
/// which may allocate or copy data. Capacity is not a memory limit.
/// [`fill_buf`](BufRead::fill_buf) reads only when the buffer is empty.
///
/// To work with data in the buffer, use [`take_view`](Self::take_view) to
/// borrow a slice or [`take_bytes`](Self::take_bytes) to take an owned
/// `BytesMut`. Both consume input without reading from the source.
/// `take_view` does not copy data; `take_bytes` may allocate or copy, as
/// described in its documentation.
///
/// The buffer can grow from an initial capacity of zero. Taking owned bytes
/// can reduce its capacity, and later reads can increase it again. Failed
/// calls to [`read_more`](Self::read_more) do not add data to the buffer.
///
/// Dropping the reader or calling [`into_inner`](Self::into_inner) discards
/// data still in the buffer without rewinding the source. Reading directly
/// from the source can therefore skip data, as can creating several buffered
/// readers for the same stream.
///
/// # Examples
///
/// ```
/// use std::io::{self, BufRead};
///
/// use bytesreader::BytesReader;
///
/// let mut reader = BytesReader::new(&b"abc\ndef\n"[..]);
/// assert_eq!(reader.fill_buf()?, b"abc\ndef\n");
/// assert_eq!(reader.take_view(..3, Some(4)), b"abc");
/// let bytes = reader.take_bytes(..3, Some(4));
/// assert!(reader.fill_buf()?.is_empty());
/// drop(reader);
/// assert_eq!(&bytes[..], b"def");
/// # Ok::<(), io::Error>(())
/// ```
pub struct BytesReader<R: ?Sized> {
    buf: BytesBuf,
    inner: R,
}

impl<R: Read> BytesReader<R> {
    /// Creates a reader with the default buffer capacity.
    ///
    /// The default is currently 8 KiB (512 bytes on ESP-IDF), but may change.
    #[inline]
    pub fn new(inner: R) -> Self {
        Self::with_capacity(DEFAULT_BUF_SIZE, inner)
    }

    /// Tries to allocate the default buffer, then calls `f` to create the source.
    ///
    /// Returns [`io::ErrorKind::OutOfMemory`] without calling `f` if allocation
    /// fails. Otherwise, calls `f` once and returns any error it produces.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::io::{self, BufRead};
    /// use bytesreader::BytesReader;
    ///
    /// let mut reader = BytesReader::try_new_with(|| Ok(&b"hello"[..]))?;
    /// assert_eq!(reader.fill_buf()?, b"hello");
    /// # Ok::<(), io::Error>(())
    /// ```
    #[inline]
    pub fn try_new_with(f: impl FnOnce() -> io::Result<R>) -> io::Result<Self> {
        let buf = BytesBuf::try_with_capacity(DEFAULT_BUF_SIZE)?;
        let inner = f()?;
        Ok(Self { buf, inner })
    }

    /// Creates a reader with room for at least `capacity` bytes.
    ///
    /// With a capacity of zero, the buffer is allocated when first filled.
    /// Reading more input before consuming existing data can grow the buffer.
    ///
    /// # Panics
    ///
    /// Panics if the requested capacity exceeds `isize::MAX` bytes.
    #[inline]
    pub fn with_capacity(capacity: usize, inner: R) -> Self {
        Self {
            buf: BytesBuf::with_capacity(capacity),
            inner,
        }
    }
}

impl<R: ?Sized> BytesReader<R> {
    /// Returns a reference to the underlying reader.
    ///
    /// Reading directly from it can skip data still in the buffer.
    #[inline]
    pub fn get_ref(&self) -> &R {
        &self.inner
    }

    /// Returns a mutable reference to the underlying reader.
    ///
    /// Reading or seeking through this reference does not update the buffer.
    /// Data already in the buffer will still be returned before new input from
    /// the source. Use `BytesReader`'s methods to keep the two in sync.
    #[inline]
    pub fn get_mut(&mut self) -> &mut R {
        &mut self.inner
    }

    /// Returns the underlying reader and discards data still in the buffer.
    ///
    /// The source is not rewound, so any data read ahead is lost.
    #[inline]
    pub fn into_inner(self) -> R
    where
        R: Sized,
    {
        self.inner
    }

    /// Returns the data remaining in the buffer.
    ///
    /// Unlike [`fill_buf`](BufRead::fill_buf), this does not attempt to fill an
    /// empty buffer.
    #[inline]
    pub fn buffer(&self) -> &[u8] {
        self.buf.buffer()
    }

    /// Returns `true` if any bytes remain in the buffer.
    ///
    /// Does not read from the source. Returning `false` does not imply EOF.
    #[inline]
    pub fn has_remaining(&self) -> bool {
        !self.buf.is_empty()
    }

    /// Returns the buffer's current capacity in bytes.
    ///
    /// Splitting off bytes with [`take_bytes`](Self::take_bytes) can reduce
    /// this value. Later reads can recover space or grow the buffer. Capacity
    /// does not include storage held by bytes already split off.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.buf.capacity()
    }

    /// Returns the bytes in `range` as a slice and consumes a prefix of the buffer.
    ///
    /// Pass `None` to consume through the end of the range, or `Some(n)` to
    /// consume exactly `n` bytes. `Some(0)` keeps all input in the buffer.
    /// Both arguments refer to [`buffer`](Self::buffer) before the call.
    /// For example, `take_view(1..4, Some(5))` returns `abc` from `<abc>tail`
    /// and leaves `tail`.
    ///
    /// No bytes are copied and no input is read. The slice can include bytes
    /// that remain in the buffer. Finish using it before reading or consuming
    /// more input.
    ///
    /// # Panics
    ///
    /// Panics if the range is invalid or you ask to consume more bytes than remain.
    /// No bytes are consumed in either case.
    ///
    /// ```compile_fail
    /// use std::io::BufRead;
    /// use bytesreader::BytesReader;
    /// let mut reader = BytesReader::new(&b"ab"[..]);
    /// reader.fill_buf().unwrap();
    /// let view = reader.take_view(..1, None);
    /// reader.read_more().unwrap();
    /// println!("{view:?}"); // The view still borrows the reader.
    /// ```
    #[inline]
    pub fn take_view(&mut self, range: impl RangeBounds<usize>, consume: Option<usize>) -> &[u8] {
        self.buf.take_view(range, consume)
    }

    /// Returns the bytes in `range` as an owned [`BytesMut`] and consumes a prefix.
    ///
    /// Pass `None` to consume through the end of the range, or `Some(n)` to
    /// consume exactly `n` bytes. Both arguments refer to
    /// [`buffer`](Self::buffer) before the call.
    ///
    /// If you consume through the end of the range, the selected bytes are
    /// split off without copying. If some of them must stay in the buffer,
    /// they are copied into new storage instead. Splitting can also allocate.
    /// This method does not read from the source.
    ///
    /// You can keep the result across further reads or after dropping the reader.
    ///
    /// # Panics
    ///
    /// Panics if the range is invalid or you ask to consume more bytes than remain.
    /// No bytes are consumed in either case.
    #[inline]
    pub fn take_bytes(
        &mut self,
        range: impl RangeBounds<usize>,
        consume: Option<usize>,
    ) -> BytesMut {
        self.buf.take_bytes(range, consume)
    }
}

impl<R: ?Sized + Read> BytesReader<R> {
    /// Reads more input into the buffer, keeping the data already there.
    ///
    /// Reads from the source once and returns the number of bytes added.
    /// If it returns zero, existing data stays in the buffer. You can try
    /// reading again later.
    ///
    /// The buffer grows if more space is needed, which may allocate or copy
    /// data. Making room can discard previously consumed bytes, even when
    /// the read fails.
    ///
    /// With the `nightly` feature, uses `Read::read_buf` on the source.
    /// Otherwise, uses [`Read::read`].
    ///
    /// # Errors
    ///
    /// Returns errors from the source, including [`io::ErrorKind::Interrupted`],
    /// without retrying. Bytes from a failed read are not added to the buffer.
    ///
    /// # Panics
    ///
    /// Panics if the required capacity exceeds `isize::MAX` bytes, the
    /// source panics, or `read` reports more bytes than requested.
    #[inline]
    pub fn read_more(&mut self) -> io::Result<usize> {
        self.buf.read_from(&mut self.inner)
    }
}

impl<R: ?Sized + Seek> BytesReader<R> {
    /// Seeks by `offset` bytes relative to the current position.
    ///
    /// If the destination is within the buffer, moves to it without seeking
    /// the source. This can make previously consumed bytes available again.
    /// Otherwise, seeks the source and discards the buffer.
    ///
    /// # Errors
    ///
    /// Returns source seek errors when the destination is outside the buffer.
    /// See [`seek`](Seek::seek) for the effect of a failed seek.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::io::{self, Cursor, Read, Seek};
    /// use bytesreader::BytesReader;
    ///
    /// let mut reader = BytesReader::with_capacity(8, Cursor::new(b"abcdefgh"));
    /// reader.read_exact(&mut [0; 3])?;
    /// reader.seek_relative(-2)?;
    /// assert_eq!(reader.stream_position()?, 1);
    /// assert_eq!(reader.get_ref().position(), 8);
    /// # Ok::<(), io::Error>(())
    /// ```
    #[inline]
    pub fn seek_relative(&mut self, offset: i64) -> io::Result<()> {
        // consumed is the current position within this buffer; filled marks
        // the end of its data. These correspond to BufReader's pos and filled.
        let pos = self.buf.consumed() as u64;
        if offset < 0 {
            // Use unsigned_abs rather than -offset so i64::MIN cannot overflow.
            let back = offset.unsigned_abs();
            if pos.checked_sub(back).is_some() {
                self.buf.unconsume(back as usize);
                return Ok(());
            }
        } else if let Some(new_pos) = pos.checked_add(offset as u64)
            && new_pos <= self.buf.filled() as u64
        {
            self.buf.consume(offset as usize);
            return Ok(());
        }

        // The destination is outside the buffer. seek accounts for read-ahead
        // before moving the source and discarding the remaining bytes.
        self.seek(SeekFrom::Current(offset)).map(drop)
    }
}

impl<R: ?Sized + Read> BufRead for BytesReader<R> {
    /// Returns a slice of the buffer, filling it with one read if it is empty.
    ///
    /// An empty slice indicates that the source returned zero bytes. A later
    /// call can read again. See [`read_more`](Self::read_more) for errors.
    #[inline]
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        // Keep returning the same data until it has all been consumed.
        // read_more reuses consumed space before asking the source for input.
        if self.buf.is_empty() {
            self.read_more()?;
        }
        Ok(self.buffer())
    }

    /// Consumes up to `amount` bytes from the buffer without reading.
    ///
    /// As with [`BufRead::consume`], requesting more bytes than the buffer
    /// contains is a logic error. This implementation consumes the entire
    /// buffer in that case.
    #[inline]
    fn consume(&mut self, amount: usize) {
        self.buf.consume(amount.min(self.buf.remaining()));
    }
}

impl<R: ?Sized + Read> Read for BytesReader<R> {
    #[inline]
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // If we don't have any remaining data and we're doing a massive read
        // (larger than our internal buffer), bypass our internal buffer
        // entirely.
        if self.buf.is_empty() && buf.len() >= self.capacity() {
            self.buf.clear();
            return self.inner.read(buf);
        }
        let mut rem = self.fill_buf()?;
        let nread = rem.read(buf)?;
        self.consume(nread);
        Ok(nread)
    }

    #[inline]
    fn read_vectored(&mut self, bufs: &mut [IoSliceMut<'_>]) -> io::Result<usize> {
        // As in read, bypass an empty buffer when the destinations can hold a
        // full batch. Forwarding preserves the source's vectored implementation.
        let total_len = bufs.iter().map(|b| b.len()).sum::<usize>();
        if self.buf.is_empty() && total_len >= self.capacity() {
            self.buf.clear();
            return self.inner.read_vectored(bufs);
        }

        // The slice implementation fills the destinations in order and reports
        // the total copied. Advance our buffer by exactly that amount.
        let mut rem = self.fill_buf()?;
        let nread = rem.read_vectored(bufs)?;
        self.consume(nread);
        Ok(nread)
    }

    // Small read_exacts are common in parsers. Handle a field already in the
    // buffer with one copy instead of entering the default read loop.
    #[inline]
    fn read_exact(&mut self, mut buf: &mut [u8]) -> io::Result<()> {
        if self
            .buf
            .consume_with(buf.len(), |claimed| buf.copy_from_slice(claimed))
        {
            return Ok(());
        }

        // Continue across short reads and retry interruptions until the entire
        // destination is filled. EOF before completion is an error.
        while !buf.is_empty() {
            match self.read(buf) {
                Ok(0) => break,
                Ok(n) => buf = &mut buf[n..],
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        if !buf.is_empty() {
            Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "failed to fill whole buffer",
            ))
        } else {
            Ok(())
        }
    }

    // The source may have an optimized read_to_end, such as File's use of the
    // file size. Drain the buffer first, then let the source read the remainder.
    fn read_to_end(&mut self, buf: &mut Vec<u8>) -> io::Result<usize> {
        let inner_buf = self.buffer();
        buf.try_reserve(inner_buf.len())?;
        buf.extend_from_slice(inner_buf);
        let nread = inner_buf.len();
        self.buf.clear();
        Ok(nread + self.inner.read_to_end(buf)?)
    }

    fn read_to_string(&mut self, buf: &mut String) -> io::Result<usize> {
        struct Guard<'a> {
            buf: &'a mut Vec<u8>,
            len: usize,
        }

        impl Drop for Guard<'_> {
            fn drop(&mut self) {
                // SAFETY: len stays zero until all bytes pass UTF-8 validation.
                // Then it equals buf.len(), with no further mutation. Both are
                // valid lengths; on unwinding, zero discards unvalidated bytes.
                unsafe { self.buf.set_len(self.len) };
            }
        }

        // With existing text we must read into a separate Vec, validate the new
        // bytes, and append them. For an empty String, read directly into its
        // allocation to avoid that extra allocation and copy.
        if buf.is_empty() {
            let mut guard = Guard {
                len: 0,
                // SAFETY: the String is empty. The guard restores that valid
                // length on panic or invalid UTF-8, before the borrow ends.
                // Only validation of all resulting bytes permits a longer String.
                buf: unsafe { buf.as_mut_vec() },
            };
            let result = self.read_to_end(guard.buf);
            if str::from_utf8(guard.buf).is_err() {
                // Prefer the source's I/O error when reading and validation fail.
                result.and_then(|_| {
                    Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "stream did not contain valid UTF-8",
                    ))
                })
            } else {
                // Keep valid partial text even if reading returned an error.
                guard.len = guard.buf.len();
                result
            }
        } else {
            // The buffer may end halfway through a UTF-8 character. Read the
            // complete suffix before validating it and appending to the String.
            let mut bytes = Vec::new();
            self.read_to_end(&mut bytes)?;
            let string = str::from_utf8(&bytes).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "stream did not contain valid UTF-8",
                )
            })?;
            *buf += string;
            Ok(string.len())
        }
    }

    #[cfg(feature = "nightly")]
    #[inline]
    fn read_buf(&mut self, mut cursor: BorrowedCursor<'_, u8>) -> io::Result<()> {
        // If we don't have any remaining data and we're doing a massive read
        // (larger than our internal buffer), bypass our internal buffer
        // entirely.
        if self.buf.is_empty() && cursor.capacity() >= self.capacity() {
            self.buf.clear();
            return self.inner.read_buf(cursor);
        }

        let prev = cursor.written();
        let mut rem = self.fill_buf()?;
        rem.read_buf(cursor.reborrow())?;
        // The slice implementation only appends to the cursor and cannot fail.
        // Count the bytes appended by this call, excluding earlier cursor data.
        self.consume(cursor.written() - prev);
        Ok(())
    }

    #[cfg(feature = "nightly")]
    #[inline]
    fn read_buf_exact(&mut self, mut cursor: BorrowedCursor<'_, u8>) -> io::Result<()> {
        if self
            .buf
            .consume_with(cursor.capacity(), |claimed| cursor.append(claimed))
        {
            return Ok(());
        }

        // Preserve cursor progress across short reads and interruptions.
        // A successful call that writes nothing indicates EOF.
        while cursor.capacity() > 0 {
            let prev_written = cursor.written();
            match self.read_buf(cursor.reborrow()) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
            if cursor.written() == prev_written {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "failed to fill whole buffer",
                ));
            }
        }
        Ok(())
    }
}

impl<R: ?Sized + fmt::Debug> fmt::Debug for BytesReader<R> {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt.debug_struct("BytesReader")
            .field("reader", &&self.inner)
            .field(
                "buffer",
                &format_args!("{}/{}", self.buf.remaining(), self.capacity()),
            )
            .finish()
    }
}

impl<R: ?Sized + Seek> Seek for BytesReader<R> {
    /// Seeks to a byte position, discarding remaining data on success.
    ///
    /// [`SeekFrom::Current`] is relative to the caller's position, accounting
    /// for input already read ahead of it. After success,
    /// [`into_inner`](Self::into_inner) returns the source at the requested
    /// position. Use [`seek_relative`](Self::seek_relative) to seek within
    /// the buffer, including consumed bytes, without discarding its contents.
    ///
    /// # Errors
    ///
    /// Returns errors from the source. A failed seek normally retains the
    /// buffer. The source's position after failure depends on its implementation.
    /// If adjusting a relative offset would overflow `i64`, two seeks are
    /// needed. Once the first succeeds, the buffer is discarded even if the
    /// second fails.
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let result;
        if let SeekFrom::Current(n) = pos {
            // The source is ahead of the caller by the remaining data length.
            // Subtract that amount to make Current relative to the caller.
            let remainder = self.buf.remaining() as i64;
            // A byte buffer fits in isize::MAX, so remainder fits in i64.
            // However, n can be i64::MIN, so the subtraction can underflow.
            if let Some(offset) = n.checked_sub(remainder) {
                result = self.inner.seek(SeekFrom::Current(offset))?;
            } else {
                // First move back to the caller's position, then apply n.
                // After the first seek succeeds, discard the buffer even if
                // the second fails: the source no longer sits after those bytes.
                self.inner.seek(SeekFrom::Current(-remainder))?;
                self.buf.clear();
                result = self.inner.seek(SeekFrom::Current(n))?;
            }
        } else {
            // Seeking with Start/End doesn't care about our buffer length.
            result = self.inner.seek(pos)?;
        }
        self.buf.clear();
        Ok(result)
    }

    /// Returns the current byte position without discarding remaining data.
    ///
    /// The source returned by [`into_inner`](Self::into_inner) can be ahead
    /// of this position. Use [`seek`](Seek::seek) with `SeekFrom::Current(0)`
    /// when the source itself must be positioned at the next byte to read.
    ///
    /// # Errors
    ///
    /// Returns errors from the source's position query.
    ///
    /// # Panics
    ///
    /// Panics if the source reports a position smaller than the remaining
    /// length, for example after seeking it directly through
    /// [`get_mut`](Self::get_mut).
    fn stream_position(&mut self) -> io::Result<u64> {
        // Correct for read-ahead without seeking or discarding remaining data.
        let remainder = self.buf.remaining() as u64;
        self.inner.stream_position().map(|pos| {
            pos.checked_sub(remainder).expect(
                "overflow when subtracting remaining buffer size from inner stream position",
            )
        })
    }

    #[inline]
    fn seek_relative(&mut self, offset: i64) -> io::Result<()> {
        self.seek_relative(offset)
    }
}

#[cfg(test)]
mod test {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;
    use std::collections::VecDeque;
    use std::io::{self, BufRead, BufReader, Cursor, IoSliceMut, Read, Seek, SeekFrom};
    use std::ops::Bound::{Excluded, Included, Unbounded};
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use super::{BytesReader, DEFAULT_BUF_SIZE};

    // Count only on the calling thread so parallel tests do not affect each
    // other.
    thread_local! {
        static TRACK: Cell<bool> = const { Cell::new(false) };
        pub(crate) static COUNT: Cell<usize> = const { Cell::new(0) };
        pub(crate) static FAIL_NEXT_ALLOCATION: Cell<bool> = const { Cell::new(false) };
    }

    struct Allocator;

    // SAFETY: successful operations forward the caller's pointer/layout to
    // System. An injected failure returns null without touching an existing
    // allocation. The const thread-local Cells do not allocate.
    unsafe impl GlobalAlloc for Allocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            if TRACK.try_with(Cell::get).unwrap_or(false) {
                COUNT.with(|n| n.set(n.get() + 1));
            }
            if FAIL_NEXT_ALLOCATION
                .try_with(|fail| fail.replace(false))
                .unwrap_or(false)
            {
                return std::ptr::null_mut();
            }
            unsafe { System.alloc(layout) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { System.dealloc(ptr, layout) }
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
            if TRACK.try_with(Cell::get).unwrap_or(false) {
                COUNT.with(|n| n.set(n.get() + 1));
            }
            if FAIL_NEXT_ALLOCATION
                .try_with(|fail| fail.replace(false))
                .unwrap_or(false)
            {
                return std::ptr::null_mut();
            }
            unsafe { System.realloc(ptr, layout, size) }
        }
    }

    #[global_allocator]
    static ALLOCATOR: Allocator = Allocator;

    // Return the result and number of allocation/reallocation calls during the
    // operation on this thread. Calls must not be nested: each resets the
    // counter.
    pub(crate) fn allocations<T>(operation: impl FnOnce() -> T) -> (T, usize) {
        struct Stop;
        impl Drop for Stop {
            fn drop(&mut self) {
                TRACK.with(|track| track.set(false));
            }
        }

        COUNT.with(|count| count.set(0));
        TRACK.with(|track| track.set(true));
        let stop = Stop;
        let result = operation();
        // Stop tracking even when an assertion inside operation unwinds.
        drop(stop);
        (result, COUNT.with(Cell::get))
    }

    struct Source<'a> {
        data: &'a [u8],
        chunk: usize,
        calls: usize,
        destination: usize,
    }

    impl Read for Source<'_> {
        fn read(&mut self, dst: &mut [u8]) -> io::Result<usize> {
            self.calls += 1;
            self.destination = dst.as_ptr() as usize;
            let len = dst.len().min(self.chunk);
            self.data.read(&mut dst[..len])
        }
    }

    fn source(data: &[u8], chunk: usize) -> Source<'_> {
        Source {
            data,
            chunk,
            calls: 0,
            destination: 0,
        }
    }

    #[test]
    fn try_new_with_allocates_before_creating_the_source() {
        let ((mut reader, allocations_before_source), total_allocations) = allocations(|| {
            let mut allocations_before_source = 0;
            let reader = BytesReader::try_new_with(|| {
                allocations_before_source = COUNT.with(Cell::get);
                Ok(source(b"hello", 8))
            })
            .unwrap();
            (reader, allocations_before_source)
        });
        assert_eq!(allocations_before_source, 1);
        assert_eq!(total_allocations, 1);
        assert_eq!(reader.capacity(), DEFAULT_BUF_SIZE);
        assert!(!reader.has_remaining());
        assert_eq!(reader.get_ref().calls, 0);
        assert_eq!(reader.fill_buf().unwrap(), b"hello");
    }

    #[test]
    fn try_new_with_returns_source_error() {
        let error = io::Error::from_raw_os_error(1234);
        let result = BytesReader::<&[u8]>::try_new_with(|| Err(error));
        assert_eq!(result.unwrap_err().raw_os_error(), Some(1234));
    }

    #[test]
    fn try_new_with_does_not_create_source_if_allocation_fails() {
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                FAIL_NEXT_ALLOCATION.with(|fail| fail.set(false));
            }
        }
        let reset = Reset;
        let mut called = false;
        FAIL_NEXT_ALLOCATION.with(|fail| fail.set(true));
        let result = BytesReader::try_new_with(|| {
            called = true;
            Ok(&b"hello"[..])
        });
        drop(reset);
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::OutOfMemory);
        assert!(!called);
    }

    #[test]
    fn fill_buf_only_reads_when_empty_and_take_methods_do_not_read() {
        let mut reader = BytesReader::with_capacity(8, source(b"abcdefghijk", 8));
        assert!(!reader.has_remaining());
        assert_eq!(reader.take_view(.., None), b"");
        assert!(reader.take_bytes(.., None).is_empty());
        assert_eq!(reader.get_ref().calls, 0);
        assert_eq!(reader.fill_buf().unwrap(), b"abcdefgh");
        assert!(reader.has_remaining());
        assert_eq!(
            reader.buffer().as_ptr() as usize,
            reader.get_ref().destination
        );
        assert_eq!(reader.take_view(..2, None), b"ab");
        assert_eq!(reader.fill_buf().unwrap(), b"cdefgh");
        assert_eq!(&reader.take_bytes(..2, None)[..], b"cd");
        assert_eq!(reader.get_ref().calls, 1);
        reader.consume(4);
        assert!(!reader.has_remaining());
        assert_eq!(reader.get_ref().calls, 1);
        assert_eq!(reader.fill_buf().unwrap(), b"ijk");
        assert!(reader.has_remaining());
        assert_eq!(reader.get_ref().calls, 2);
    }

    #[test]
    fn read_drains_buffer_before_using_direct_read() {
        let mut reader = BytesReader::with_capacity(4, source(b"abcdefghijkl", 64));
        assert_eq!(reader.fill_buf().unwrap(), b"abcd");
        reader.consume(1);
        let mut dst = [0; 8];
        assert_eq!(reader.read(&mut dst).unwrap(), 3);
        assert_eq!(&dst[..3], b"bcd");
        assert_eq!(reader.get_ref().calls, 1);
        assert_eq!(reader.read(&mut dst).unwrap(), 8);
        assert_eq!(&dst, b"efghijkl");
        assert_eq!(reader.get_ref().destination, dst.as_ptr() as usize);
        assert!(reader.buffer().is_empty());
    }

    #[test]
    fn empty_read_matches_bufreader_and_does_not_consume_buffered_data() {
        for capacity in [0, 8] {
            let mut reader = BytesReader::with_capacity(capacity, source(b"abc", 8));
            let mut reference = std::io::BufReader::with_capacity(capacity, source(b"abc", 8));
            assert_eq!(reader.read(&mut []).unwrap(), 0);
            assert_eq!(reference.read(&mut []).unwrap(), 0);
            assert_eq!(reader.get_ref().calls, reference.get_ref().calls);
            assert_eq!(reader.buffer(), reference.buffer());
            reader.fill_buf().unwrap();
            let remaining = reader.buffer().to_vec();
            let calls = reader.get_ref().calls;
            assert_eq!(reader.read(&mut []).unwrap(), 0);
            assert_eq!(reader.buffer(), remaining);
            assert_eq!(reader.get_ref().calls, calls);
        }
    }

    #[test]
    fn vectored_reads_scatter_buffered_data_and_forward_large_reads() {
        struct Vectored<'a> {
            data: &'a [u8],
            scalar: usize,
            vectored: usize,
        }
        impl Read for Vectored<'_> {
            fn read(&mut self, dst: &mut [u8]) -> io::Result<usize> {
                self.scalar += 1;
                self.data.read(dst)
            }
            fn read_vectored(&mut self, dst: &mut [IoSliceMut<'_>]) -> io::Result<usize> {
                self.vectored += 1;
                self.data.read_vectored(dst)
            }
        }
        let mut reader = BytesReader::with_capacity(
            4,
            Vectored {
                data: b"abcdefghijklmnop",
                scalar: 0,
                vectored: 0,
            },
        );
        reader.fill_buf().unwrap();
        reader.consume(1);
        let mut a = [0; 2];
        let mut b = [0; 3];
        assert_eq!(
            reader
                .read_vectored(&mut [
                    IoSliceMut::new(&mut []),
                    IoSliceMut::new(&mut a),
                    IoSliceMut::new(&mut b),
                ])
                .unwrap(),
            3
        );
        assert_eq!(&a, b"bc");
        assert_eq!(b, [b'd', 0, 0]);
        assert_eq!((reader.get_ref().scalar, reader.get_ref().vectored), (1, 0));
        assert_eq!(
            reader
                .read_vectored(&mut [IoSliceMut::new(&mut a), IoSliceMut::new(&mut b),])
                .unwrap(),
            5
        );
        assert_eq!(&a, b"ef");
        assert_eq!(&b, b"ghi");
        assert_eq!((reader.get_ref().scalar, reader.get_ref().vectored), (1, 1));
        reader.read_exact(&mut a).unwrap();
        let calls = reader.get_ref().scalar;
        reader.read_exact(&mut a).unwrap();
        assert_eq!(&a, b"lm");
        reader.read_exact(&mut []).unwrap();
        assert_eq!(reader.get_ref().scalar, calls);
    }

    #[test]
    fn bulk_read_helpers_match_bufreader_on_partial_input_and_utf8_errors() {
        fn script(bytes: &'static [u8], fail: bool) -> Script {
            let mut steps = VecDeque::from([Step::Data(bytes)]);
            if fail {
                steps.push_back(Step::Error(io::ErrorKind::Other));
            }
            Script(steps)
        }
        for bytes in ["ab€文".as_bytes(), &b"ab\xffcd"[..], &b"ab\xe2\x82"[..]] {
            for capacity in [1, 3, 32] {
                for fail in [false, true] {
                    for prefix in ["", "prefix"] {
                        let mut reader = BytesReader::with_capacity(capacity, script(bytes, fail));
                        let mut reference = BufReader::with_capacity(capacity, script(bytes, fail));
                        reader.fill_buf().unwrap();
                        reference.fill_buf().unwrap();
                        let mut actual = prefix.to_owned();
                        let mut expected = prefix.to_owned();
                        assert_eq!(
                            reader.read_to_string(&mut actual).map_err(|e| e.kind()),
                            reference
                                .read_to_string(&mut expected)
                                .map_err(|e| e.kind()),
                        );
                        assert_eq!(actual, expected);
                        assert_eq!(reader.buffer(), reference.buffer());
                    }
                    let mut reader = BytesReader::with_capacity(capacity, script(bytes, fail));
                    let mut reference = BufReader::with_capacity(capacity, script(bytes, fail));
                    reader.fill_buf().unwrap();
                    reference.fill_buf().unwrap();
                    let mut actual = b"prefix".to_vec();
                    let mut expected = actual.clone();
                    assert_eq!(
                        reader.read_to_end(&mut actual).map_err(|e| e.kind()),
                        reference.read_to_end(&mut expected).map_err(|e| e.kind()),
                    );
                    assert_eq!(actual, expected);
                    assert_eq!(reader.buffer(), reference.buffer());
                }
            }
        }
    }

    #[test]
    fn read_to_string_remains_valid_if_source_overwrites_prefix_or_panics() {
        struct Mutating(bool);
        impl Read for Mutating {
            fn read(&mut self, dst: &mut [u8]) -> io::Result<usize> {
                (&b"abc"[..]).read(dst)
            }
            fn read_to_end(&mut self, dst: &mut Vec<u8>) -> io::Result<usize> {
                dst.fill(0xff);
                if self.0 {
                    panic!("read_to_end panic");
                }
                Ok(0)
            }
        }
        for panics in [false, true] {
            for prefix in ["", "prefix"] {
                let mut reader = BytesReader::with_capacity(3, Mutating(panics));
                reader.fill_buf().unwrap();
                let mut text = prefix.to_owned();
                let result = catch_unwind(AssertUnwindSafe(|| reader.read_to_string(&mut text)));
                if panics {
                    assert!(result.is_err());
                } else {
                    assert_eq!(
                        result.unwrap().unwrap_err().kind(),
                        io::ErrorKind::InvalidData
                    );
                }
                assert_eq!(text, prefix);
                assert!(reader.buffer().is_empty());
            }
        }
    }

    #[test]
    fn seek_methods_match_bufreader_and_keep_retained_bytes_independent() {
        let data = b"abcdefghijklmnop";
        let mut reader = BytesReader::with_capacity(8, Cursor::new(data));
        let mut reference = BufReader::with_capacity(8, Cursor::new(data));
        for offset in [0, 3, -2, 7, -8, 10, -3, -4, 2] {
            reader.fill_buf().unwrap();
            reference.fill_buf().unwrap();
            Seek::seek_relative(&mut reader, offset).unwrap();
            Seek::seek_relative(&mut reference, offset).unwrap();
            assert_eq!(
                reader.stream_position().unwrap(),
                reference.stream_position().unwrap()
            );
            assert_eq!(reader.buffer(), reference.buffer());
            assert_eq!(reader.get_ref().position(), reference.get_ref().position());
        }
        for position in [SeekFrom::Current(0), SeekFrom::Start(2), SeekFrom::End(-3)] {
            reader.fill_buf().unwrap();
            reference.fill_buf().unwrap();
            assert_eq!(
                reader.seek(position).unwrap(),
                reference.seek(position).unwrap()
            );
            assert!(reader.buffer().is_empty());
            assert_eq!(reader.fill_buf().unwrap(), reference.fill_buf().unwrap());
        }
        reader.rewind().unwrap();
        reader.fill_buf().unwrap();
        let mut retained = reader.take_bytes(1..3, Some(5));
        retained.fill(b'!');
        reader.seek_relative(-2).unwrap();
        assert_eq!(reader.buffer(), b"defgh");
        // The earlier bytes have been split off, so this seek must use the source.
        reader.seek_relative(-2).unwrap();
        assert_eq!(reader.stream_position().unwrap(), 1);
        let mut bytes = [0; 3];
        reader.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"bcd");
        assert_eq!(&retained[..], b"!!");
    }

    #[test]
    fn seek_failure_preserves_buffer_unless_an_overflow_adjustment_succeeded() {
        struct Virtual {
            position: u64,
            attempts: usize,
            fail: usize,
        }
        impl Read for Virtual {
            fn read(&mut self, dst: &mut [u8]) -> io::Result<usize> {
                dst.fill(b'x');
                self.position += dst.len() as u64;
                Ok(dst.len())
            }
        }
        impl Seek for Virtual {
            fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
                self.attempts += 1;
                if self.attempts == self.fail {
                    return Err(io::ErrorKind::Other.into());
                }
                let next = match position {
                    SeekFrom::Start(pos) => pos as i128,
                    SeekFrom::Current(offset) => self.position as i128 + offset as i128,
                    SeekFrom::End(_) => unreachable!(),
                };
                self.position = u64::try_from(next).map_err(|_| io::ErrorKind::InvalidInput)?;
                Ok(self.position)
            }
        }
        for relative in [false, true] {
            for fail in [0, 1, 2] {
                let mut reader = BytesReader::with_capacity(
                    8,
                    Virtual {
                        position: 1_u64 << 63,
                        attempts: 0,
                        fail,
                    },
                );
                reader.fill_buf().unwrap();
                let result = if relative {
                    reader
                        .seek_relative(i64::MIN)
                        .map(|_| reader.get_ref().position)
                } else {
                    reader.seek(SeekFrom::Current(i64::MIN))
                };
                assert_eq!(result.is_ok(), fail == 0);
                assert_eq!(reader.buffer().len(), if fail == 1 { 8 } else { 0 });
                assert_eq!(reader.get_ref().attempts, if fail == 1 { 1 } else { 2 });
                if fail == 0 {
                    assert_eq!(result.unwrap(), 0);
                }
            }
        }
    }

    #[test]
    fn inherited_read_and_bufread_adapters_remain_available() {
        let mut reader = BytesReader::with_capacity(2, &b"abc\0de\nfg"[..]);
        assert_eq!(reader.skip_until(0).unwrap(), 4);
        let mut line = String::new();
        assert_eq!(reader.read_line(&mut line).unwrap(), 3);
        assert_eq!(line, "de\n");
        let mut first = Vec::new();
        reader.by_ref().take(1).read_to_end(&mut first).unwrap();
        assert_eq!(first, b"f");
        assert_eq!(
            reader
                .chain(&b"hi"[..])
                .bytes()
                .collect::<io::Result<Vec<_>>>()
                .unwrap(),
            b"ghi"
        );
        let reader = BytesReader::with_capacity(2, &b"a,b,,c"[..]);
        assert_eq!(
            reader.split(b',').collect::<io::Result<Vec<_>>>().unwrap(),
            [b"a".to_vec(), b"b".to_vec(), vec![], b"c".to_vec()]
        );
    }

    #[test]
    fn read_more_appends_a_partial_record_and_can_grow() {
        let mut reader = BytesReader::with_capacity(2, source(b"abcdefghij", 2));
        while reader.buffer().len() < 10 {
            assert!(reader.read_more().unwrap() > 0);
        }
        assert_eq!(reader.buffer(), b"abcdefghij");
        assert!(reader.capacity() >= 10);
        assert_eq!(reader.read_more().unwrap(), 0);
        assert_eq!(reader.take_view(1..=7, Some(9)), b"bcdefgh");
        assert_eq!(reader.buffer(), b"j");
    }

    enum Step {
        Data(&'static [u8]),
        Error(io::ErrorKind),
        Panic,
        Eof,
    }

    struct Script(VecDeque<Step>);
    impl Read for Script {
        fn read(&mut self, dst: &mut [u8]) -> io::Result<usize> {
            match self.0.pop_front().unwrap_or(Step::Eof) {
                Step::Data(mut data) => {
                    let n = data.read(dst)?;
                    if !data.is_empty() {
                        self.0.push_front(Step::Data(data));
                    }
                    Ok(n)
                }
                Step::Error(kind) => {
                    dst.fill(b'?');
                    Err(kind.into())
                }
                Step::Panic => {
                    dst.fill(b'!');
                    panic!("source panic");
                }
                Step::Eof => Ok(0),
            }
        }
    }

    #[test]
    fn appending_preserves_input_after_errors_panics_and_temporary_eof() {
        let script = Script(VecDeque::from([
            Step::Data(b"abc"),
            Step::Error(io::ErrorKind::Interrupted),
            Step::Error(io::ErrorKind::Other),
            Step::Panic,
            Step::Eof,
            Step::Data(b"XYZ"),
        ]));
        let mut reader = BytesReader::with_capacity(8, script);
        assert_eq!(reader.read_more().unwrap(), 3);
        reader.consume(1);
        for kind in [io::ErrorKind::Interrupted, io::ErrorKind::Other] {
            assert_eq!(reader.read_more().unwrap_err().kind(), kind);
            assert_eq!(reader.buffer(), b"bc");
        }
        assert!(catch_unwind(AssertUnwindSafe(|| reader.read_more())).is_err());
        assert_eq!(reader.buffer(), b"bc");
        assert_eq!(reader.read_more().unwrap(), 0);
        assert_eq!(reader.fill_buf().unwrap(), b"bc");
        assert_eq!(reader.read_more().unwrap(), 3);
        assert_eq!(reader.take_view(.., None), b"bcXYZ");
    }

    #[test]
    fn fill_buf_can_retry_errors_and_resume_after_eof() {
        let script = Script(VecDeque::from([
            Step::Error(io::ErrorKind::Interrupted),
            Step::Eof,
            Step::Data(b"next"),
        ]));
        let mut reader = BytesReader::with_capacity(8, script);
        assert_eq!(
            reader.fill_buf().unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
        assert!(reader.fill_buf().unwrap().is_empty());
        assert_eq!(reader.fill_buf().unwrap(), b"next");
    }

    #[test]
    fn retained_bytes_survive_refill_mutation_and_reader_drop() {
        let mut reader = BytesReader::with_capacity(8, source(b"abcdefghijklmno", 8));
        reader.fill_buf().unwrap();
        let ptr = reader.buffer().as_ptr().wrapping_add(1);
        let mut bytes = reader.take_bytes(1..3, Some(4));
        assert_eq!(bytes.as_ptr(), ptr);
        assert_eq!(&bytes[..], b"bc");
        bytes[0] = b'X';
        let view = reader.take_view(..2, None);
        assert_eq!(view, b"ef");
        let mut rest = Vec::new();
        reader.read_to_end(&mut rest).unwrap();
        assert_eq!(rest, b"ghijklmno");
        drop(reader);
        assert_eq!(&bytes[..], b"Xc");
    }

    #[test]
    fn overlapping_extractions_retain_the_correct_prefix() {
        let mut reader = BytesReader::new(&b"<abc>tail"[..]);
        reader.fill_buf().unwrap();
        let ptr = reader.buffer().as_ptr().wrapping_add(1);
        let view = reader.take_view(1..4, Some(0));
        assert_eq!(view.as_ptr(), ptr);
        let mut bytes = reader.take_bytes(1..4, Some(2));
        assert_ne!(bytes.as_ptr(), ptr);
        bytes[0] = b'X';
        assert_eq!(&bytes[..], b"Xbc");
        assert_eq!(reader.buffer(), b"bc>tail");
    }

    #[test]
    fn invalid_extraction_ranges_and_consumption_leave_input_unchanged() {
        let ranges = [
            ((Included(4), Excluded(2)), None),
            ((Unbounded, Included(usize::MAX)), None),
            ((Excluded(usize::MAX), Unbounded), None),
            ((Unbounded, Excluded(7)), Some(1)),
            ((Unbounded, Unbounded), Some(7)),
        ];
        let mut reader = BytesReader::new(&b"abcdef"[..]);
        reader.fill_buf().unwrap();
        for (range, consume) in ranges {
            assert!(
                catch_unwind(AssertUnwindSafe(|| {
                    let _ = reader.take_view(range, consume);
                }))
                .is_err()
            );
            assert_eq!(reader.buffer(), b"abcdef");
            assert!(catch_unwind(AssertUnwindSafe(|| reader.take_bytes(range, consume))).is_err());
            assert_eq!(reader.buffer(), b"abcdef");
        }
    }

    #[test]
    fn bufread_consume_clamps_to_buffered_length_like_bufreader() {
        let mut reader = BytesReader::with_capacity(8, source(b"abcdef", 8));
        let mut reference = BufReader::with_capacity(8, source(b"abcdef", 8));
        reader.fill_buf().unwrap();
        reference.fill_buf().unwrap();
        for amount in [0, 2, 7, 1] {
            reader.consume(amount);
            reference.consume(amount);
            assert_eq!(reader.buffer(), reference.buffer());
            assert_eq!(reader.get_ref().calls, 1);
        }
    }

    #[test]
    fn mixed_operations_match_source_for_short_reads_and_zero_capacity() {
        let input: Vec<_> = (0..512).map(|x| x as u8).collect();
        for capacity in [0, 1, 8, 64] {
            for chunk in [1, 3, 17, 128] {
                let mut reader = BytesReader::with_capacity(capacity, source(&input, chunk));
                let mut offset = 0;
                let mut items = Vec::new();
                let mut step = 0;
                loop {
                    let n = reader.fill_buf().unwrap().len().min(5);
                    if n == 0 {
                        break;
                    }
                    let expected = &input[offset..offset + n];
                    match step % 4 {
                        0 => assert_eq!(reader.take_view(..n, None), expected),
                        1 => {
                            let bytes = reader.take_bytes(..n, None);
                            assert_eq!(&bytes[..], expected);
                            items.push((offset, bytes));
                        }
                        2 => {
                            let mut dst = [0; 5];
                            reader.read_exact(&mut dst[..n]).unwrap();
                            assert_eq!(&dst[..n], expected);
                        }
                        _ => {
                            assert_eq!(&reader.buffer()[..n], expected);
                            reader.consume(n);
                        }
                    }
                    offset += n;
                    step += 1;
                }
                assert_eq!(offset, input.len());
                drop(reader);
                for (offset, bytes) in items {
                    assert_eq!(&bytes[..], &input[offset..offset + bytes.len()]);
                }
            }
        }
    }

    #[test]
    fn standard_read_and_bufread_helpers_work_without_format_traits() {
        let input = b"alpha\n\nlonger line\ntail";
        let expected = ["alpha", "", "longer line", "tail"];
        for capacity in [0, 1, 8, 64] {
            let reader = BytesReader::with_capacity(capacity, source(input, 3));
            assert_eq!(
                reader.lines().collect::<io::Result<Vec<_>>>().unwrap(),
                expected
            );
            let mut reader = BytesReader::with_capacity(capacity, source(input, 3));
            let mut output = Vec::new();
            while reader.read_until(b'\n', &mut output).unwrap() != 0 {}
            assert_eq!(output, input);
        }
        let script = Script(VecDeque::from([
            Step::Data(b"ab"),
            Step::Error(io::ErrorKind::Interrupted),
            Step::Data(b"cd"),
        ]));
        let mut reader = BytesReader::with_capacity(8, script);
        let mut dst = [0; 4];
        reader.read_exact(&mut dst).unwrap();
        assert_eq!(&dst, b"abcd");
        assert_eq!(
            reader.read_exact(&mut dst).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
        let mut reader = BytesReader::new(&b"bad\xff\n"[..]);
        assert_eq!(
            reader.read_line(&mut String::new()).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn accessors_and_into_inner_preserve_the_sources_read_ahead_position() {
        let mut reader = BytesReader::with_capacity(4, Cursor::new(b"abcdefgh"));
        reader.fill_buf().unwrap();
        reader.consume(1);
        assert_eq!(reader.get_ref().position(), 4);
        reader.get_mut().set_position(6);
        let mut dst = [0; 3];
        reader.read_exact(&mut dst).unwrap();
        assert_eq!(&dst, b"bcd");
        assert_eq!(reader.fill_buf().unwrap(), b"gh");
        assert_eq!(reader.into_inner().position(), 8);
    }

    #[test]
    fn unsized_sources_support_reading_and_extraction() {
        let mut reader: Box<BytesReader<dyn Read>> = Box::new(BytesReader::new(&b"abc"[..]));
        assert_eq!(reader.fill_buf().unwrap(), b"abc");
        assert_eq!(reader.take_view(..1, None), b"a");
        assert_eq!(&reader.take_bytes(..1, None)[..], b"b");
        let mut dst = [0];
        reader.read_exact(&mut dst).unwrap();
        assert_eq!(&dst, b"c");
    }

    #[test]
    fn consumed_storage_is_reused_across_batches() {
        let input = [42; 1024];
        let mut reader = BytesReader::with_capacity(64, &input[..]);
        reader.fill_buf().unwrap();
        let (_, count) = allocations(|| {
            while !reader.fill_buf().unwrap().is_empty() {
                reader.take_view(.., None);
            }
        });
        assert_eq!(count, 0);
        assert_eq!(reader.capacity(), 64);
    }

    #[test]
    fn fill_buf_reads_only_when_empty_and_can_resume_after_repeated_eof() {
        let mut reader = BytesReader::with_capacity(4, Cursor::new(b"abcdef".to_vec()));
        assert_eq!(reader.fill_buf().unwrap(), b"abcd");
        reader.consume(2);
        assert_eq!(reader.fill_buf().unwrap(), b"cd");
        assert_eq!(reader.get_ref().position(), 4);
        reader.consume(2);
        assert_eq!(reader.fill_buf().unwrap(), b"ef");
        reader.consume(2);
        for _ in 0..2 {
            assert!(reader.fill_buf().unwrap().is_empty());
        }
        reader.get_mut().get_mut().extend_from_slice(b"\xff\0");
        assert_eq!(reader.fill_buf().unwrap(), b"\xff\0");
    }

    #[test]
    fn fill_buf_errors_do_not_expose_consumed_or_failed_input() {
        let script = Script(VecDeque::from([
            Step::Data(b"old"),
            Step::Error(io::ErrorKind::Interrupted),
            Step::Data(b"ok"),
        ]));
        let mut reader = BytesReader::with_capacity(4, script);
        assert_eq!(reader.fill_buf().unwrap(), b"old");
        // Buffered input is returned without consulting even a failing source.
        assert_eq!(reader.fill_buf().unwrap(), b"old");
        reader.consume(3);
        assert_eq!(
            reader.fill_buf().unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
        assert!(reader.buffer().is_empty());
        assert_eq!(reader.fill_buf().unwrap(), b"ok");
        assert!(reader.get_ref().0.is_empty());
    }

    #[test]
    fn fill_buf_allocates_on_demand_and_preserves_split_items() {
        let input: Vec<u8> = (0..64).collect();
        for capacity in [0, 4] {
            let mut reader = BytesReader::with_capacity(capacity, &input[..]);
            let first_len = reader.fill_buf().unwrap().len();
            assert!(first_len > 0);
            let owned = reader.take_bytes(..first_len, None);
            let mut output = owned.to_vec();
            loop {
                let chunk = reader.fill_buf().unwrap();
                if chunk.is_empty() {
                    break;
                }
                output.extend_from_slice(chunk);
                let consumed = chunk.len();
                reader.consume(consumed);
            }
            assert_eq!(output, input);
            drop(reader);
            assert_eq!(&owned[..], &input[..first_len]);
        }
    }

    #[cfg(feature = "nightly")]
    #[test]
    fn read_buf_exact_handles_cached_bytes_interruptions_and_partial_eof() {
        use std::io::BorrowedBuf;
        use std::mem::MaybeUninit;

        let mut reader = BytesReader::with_capacity(8, source(b"abcdef", 8));
        reader.fill_buf().unwrap();
        let mut storage = [MaybeUninit::uninit(); 3];
        let mut output = BorrowedBuf::from(&mut storage[..]);
        output.unfilled().append(b"!");
        reader.read_buf_exact(output.unfilled()).unwrap();
        assert_eq!(output.filled(), b"!ab");
        assert_eq!(reader.get_ref().calls, 1);

        let mut storage = [MaybeUninit::uninit(); 8];
        let mut output = BorrowedBuf::from(&mut storage[..]);
        assert_eq!(
            reader.read_buf_exact(output.unfilled()).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
        assert_eq!(output.filled(), b"cdef");

        let mut reader = BytesReader::with_capacity(
            8,
            Script(VecDeque::from([
                Step::Data(b"ab"),
                Step::Error(io::ErrorKind::Interrupted),
                Step::Data(b"cd"),
            ])),
        );
        let mut storage = [MaybeUninit::uninit(); 4];
        let mut output = BorrowedBuf::from(&mut storage[..]);
        reader.read_buf_exact(output.unfilled()).unwrap();
        assert_eq!(output.filled(), b"abcd");
    }

    #[cfg(feature = "nightly")]
    #[test]
    fn native_read_buf_supports_buffering_direct_reads_and_partial_destinations() {
        use std::io::{BorrowedBuf, BorrowedCursor};
        use std::mem::MaybeUninit;

        struct Native<'a>(&'a [u8]);
        impl Read for Native<'_> {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                panic!("must use native read_buf");
            }
            fn read_buf(&mut self, mut dst: BorrowedCursor<'_, u8>) -> io::Result<()> {
                let n = dst.capacity().min(self.0.len());
                dst.append(&self.0[..n]);
                self.0 = &self.0[n..];
                Ok(())
            }
        }
        let mut reader = BytesReader::with_capacity(4, Native(b"abcdefghij"));
        let mut small = [MaybeUninit::uninit(); 3];
        let mut dst = BorrowedBuf::from(&mut small[..]);
        dst.unfilled().append(b"!");
        reader.read_buf(dst.unfilled()).unwrap();
        assert_eq!(dst.filled(), b"!ab");
        assert_eq!(reader.buffer(), b"cd");
        let mut large = [MaybeUninit::uninit(); 16];
        let mut dst = BorrowedBuf::from(&mut large[..]);
        reader.read_buf(dst.unfilled()).unwrap();
        assert_eq!(dst.filled(), b"cd");
        reader.read_buf(dst.unfilled()).unwrap();
        assert_eq!(dst.filled(), b"cdefghij");
        assert!(reader.buffer().is_empty());
        let mut empty: [u8; 0] = [];
        reader
            .read_buf(BorrowedBuf::from(&mut empty[..]).unfilled())
            .unwrap();
    }
}
