//! Sources expose bounded contiguous windows, independently of the syntax.
use std::io;
pub mod file_stream;
pub mod gz_stream;

pub trait Stream {
    /// Maximum bytes exposed in one window, and maximum record size in decoded bytes.
    fn window_size(&self) -> usize;
    /// Expose a full window at `offset`, or the remaining bytes at EOF.
    /// Forward-only sources support offsets within the current window or at
    /// its end; seekable sources also allow arbitrary offsets. Offsets refer
    /// to decoded bytes. On success, `offset()` equals the requested offset.
    fn load(&mut self, offset: u64) -> io::Result<()>;
    fn offset(&self) -> u64;
    fn bytes(&self) -> &[u8];
    /// Whether the current window ends at physical EOF. This need not imply
    /// the reader has consumed every byte in the window.
    fn is_eof(&self) -> bool;
}

/// Stable, known-length sources that can open windows at arbitrary offsets.
/// Only these sources support independent byte-range readers.
pub trait SeekableStream: Stream {
    fn len(&self) -> u64;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A safe, borrowing source for existing in-memory data. The caller owns the
/// input allocation; the reader itself still exposes bounded windows.
pub struct SliceStream<'a> {
    data: &'a [u8],
    window_size: usize,
    offset: usize,
    end: usize,
}

impl<'a> SliceStream<'a> {
    pub fn new(data: &'a [u8], window_size: usize) -> io::Result<Self> {
        check_window_size(window_size)?;
        Ok(Self {
            data,
            window_size,
            offset: 0,
            end: 0,
        })
    }
}

pub(crate) fn check_window_size(size: usize) -> io::Result<()> {
    if size == 0 || size > u32::MAX as usize || size > isize::MAX as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "window size must be in 1..=min(u32::MAX, isize::MAX)",
        ));
    }
    Ok(())
}

impl Stream for SliceStream<'_> {
    fn window_size(&self) -> usize {
        self.window_size
    }
    fn load(&mut self, offset: u64) -> io::Result<()> {
        if offset > self.len() {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        self.offset = offset as usize;
        self.end = self
            .offset
            .saturating_add(self.window_size)
            .min(self.data.len());
        Ok(())
    }
    fn offset(&self) -> u64 {
        self.offset as u64
    }
    fn bytes(&self) -> &[u8] {
        &self.data[self.offset..self.end]
    }
    fn is_eof(&self) -> bool {
        self.end == self.data.len()
    }
}

impl SeekableStream for SliceStream<'_> {
    fn len(&self) -> u64 {
        self.data.len() as u64
    }
}
