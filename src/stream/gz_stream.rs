//! Decompress as the reader advances, retaining only a sliding output window.
//! Gzip is sequential: offsets here address decompressed bytes, not compressed
//! blocks. Token slices borrow this window until the next mutable reader call.
use super::{Stream, check_window_size};
use flate2::{Decompress, FlushDecompress, Status};
use std::{
    fs::File,
    io::{self, BufRead, BufReader, Read},
    path::Path,
};

const INPUT_BUFFER: usize = 64 * 1024;

/// A bounded gzip source. Decompresses all concatenated gzip members, checking
/// headers, CRCs and sizes, without creating a decompressed file or a file-sized
/// allocation. Header names/comments are skipped rather than retained.
///
/// Memory is the configured output window + 64 KiB compressed input + inflater
/// state + one lookahead byte. Only a record's surviving tail is moved when a
/// window advances; token values are never separately copied or allocated.
///
/// This source is forward-only and intentionally does not implement
/// `SeekableStream`: arbitrary ranges of an ordinary gzip file cannot be decoded
/// independently without an index/history checkpoints.
///
/// ```compile_fail
/// use s_parser::{GzStream, NtTokenizer, QuadReader};
/// let stream = GzStream::new(&b""[..], 1024).unwrap();
/// let reader = QuadReader::for_range(stream, NtTokenizer, 0..100);
/// ```
pub struct GzStream<R: Read = File> {
    decoder: GzipDecoder<R>,
    window: Box<[u8]>,
    filled: usize,
    offset: u64,
    lookahead: Option<u8>,
    eof: bool,
    failed: bool,
}

impl GzStream<File> {
    pub fn open(path: impl AsRef<Path>, window_size: usize) -> io::Result<Self> {
        Self::new(File::open(path)?, window_size)
    }
}

impl<R: Read> GzStream<R> {
    /// Wrap a compressed byte reader. Nothing is read or decompressed until the
    /// first `load`/`QuadReader::next` call. The input must remain stable.
    pub fn new(reader: R, window_size: usize) -> io::Result<Self> {
        check_window_size(window_size)?;
        Ok(Self {
            decoder: GzipDecoder {
                input: BufReader::with_capacity(INPUT_BUFFER, reader),
                inflater: Decompress::new_gzip(15),
                member_done: false,
            },
            window: vec![0; window_size].into_boxed_slice(),
            filled: 0,
            offset: 0,
            lookahead: None,
            eof: false,
            failed: false,
        })
    }

    /// Total decoded size is known only after reaching and validating EOF.
    /// Gzip's 32-bit trailer size is not a usable length for large/multi-member files.
    pub fn decoded_len(&self) -> Option<u64> {
        self.eof.then_some(self.offset + self.filled as u64)
    }

    /// Hand the filled buffer to readers and resume inflation in the spare.
    /// Both buffers retain their allocation; only the unconsumed tail moves.
    pub(crate) fn exchange_window(&mut self, spare: &mut Box<[u8]>, consumed: usize) {
        assert_eq!(spare.len(), self.window.len());
        assert!(consumed <= self.filled);
        let tail = self.filled - consumed;
        spare[..tail].copy_from_slice(&self.window[consumed..self.filled]);
        std::mem::swap(&mut self.window, spare);
        self.filled = tail;
        self.offset += consumed as u64;
    }

    fn refill(&mut self) -> io::Result<()> {
        if let Some(byte) = self.lookahead.take() {
            self.window[self.filled] = byte;
            self.filled += 1;
        }
        while self.filled < self.window.len() && !self.eof {
            let count = self.decoder.read(&mut self.window[self.filled..])?;
            if count == 0 {
                self.eof = true;
            } else {
                self.filled += count;
            }
        }
        // Distinguish an exact-sized final record from a record needing more
        // bytes. This also validates the final trailer at an exact window end.
        if !self.eof {
            let mut byte = [0];
            if self.decoder.read(&mut byte)? == 0 {
                self.eof = true;
            } else {
                self.lookahead = Some(byte[0]);
            }
        }
        self.offset
            .checked_add(self.filled as u64)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "decoded offset overflow"))?;
        Ok(())
    }
}

impl<R: Read> Stream for GzStream<R> {
    fn window_size(&self) -> usize {
        self.window.len()
    }
    fn offset(&self) -> u64 {
        self.offset
    }
    fn bytes(&self) -> &[u8] {
        &self.window[..self.filled]
    }
    fn is_eof(&self) -> bool {
        self.eof
    }

    fn load(&mut self, offset: u64) -> io::Result<()> {
        if self.failed {
            return Err(io::Error::other("gzip stream previously failed"));
        }
        let advance = offset
            .checked_sub(self.offset)
            .filter(|delta| *delta <= self.filled as u64)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "gzip offsets must be within the current window or at its end",
                )
            })? as usize;
        if advance == 0 && (self.filled == self.window.len() || self.eof) {
            return Ok(());
        }
        self.window.copy_within(advance..self.filled, 0);
        self.filled -= advance;
        self.offset = offset;
        if let Err(error) = self.refill() {
            self.failed = true;
            return Err(error);
        }
        Ok(())
    }
}

// Use flate2's streaming gzip inflater directly. The higher-level GzDecoder
// stores optional header strings; an unbounded filename/comment would defeat
// our memory contract. zlib-rs validates/skips these fields without storing them.
struct GzipDecoder<R> {
    input: BufReader<R>,
    inflater: Decompress,
    member_done: bool,
}

impl<R: Read> Read for GzipDecoder<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        loop {
            let input = match self.input.fill_buf() {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                result => result?,
            };
            if self.member_done {
                if input.is_empty() {
                    return Ok(0);
                }
                // Preserve gzip framing for each member; reset(true) would
                // switch to zlib framing. The old state is released here.
                self.inflater = Decompress::new_gzip(15);
                self.member_done = false;
            }
            let before_in = self.inflater.total_in();
            let before_out = self.inflater.total_out();
            let status = self
                .inflater
                .decompress(input, output, FlushDecompress::None)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            let consumed = (self.inflater.total_in() - before_in) as usize;
            let produced = (self.inflater.total_out() - before_out) as usize;
            let input_ended = input.is_empty();
            self.input.consume(consumed);
            self.member_done = status == Status::StreamEnd;
            if produced != 0 {
                return Ok(produced);
            }
            if self.member_done {
                continue;
            }
            if consumed == 0 {
                return Err(io::Error::new(
                    if input_ended {
                        io::ErrorKind::UnexpectedEof
                    } else {
                        io::ErrorKind::InvalidData
                    },
                    if input_ended {
                        "truncated gzip stream"
                    } else {
                        "gzip inflater made no progress"
                    },
                ));
            }
        }
    }
}
