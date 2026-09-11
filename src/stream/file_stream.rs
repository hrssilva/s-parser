use super::{SeekableStream, Stream, check_window_size};
use memmap2::{Mmap, MmapOptions};
use std::{fs::File, io, path::Path, sync::Arc};

/// A shared file handle with one private, bounded mapping per reader.
/// Cloning shares the handle but starts with no mapped pages.
pub struct FileStream {
    file: Arc<File>,
    len: u64,
    window_size: usize,
    offset: u64,
    map: Option<Mmap>,
}

impl FileStream {
    /// Open an immutable regular file for zero-copy parsing.
    ///
    /// # Safety
    /// The file must not be modified or truncated by any process until this
    /// stream and all its clones/readers have been dropped. A read-only handle
    /// does not enforce this. This is the safety requirement of file-backed mmap.
    pub unsafe fn open(path: impl AsRef<Path>, window_size: usize) -> io::Result<Self> {
        check_window_size(window_size)?;
        let file = File::open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "expected a regular file",
            ));
        }
        Ok(Self {
            file: Arc::new(file),
            len: metadata.len(),
            window_size,
            offset: 0,
            map: None,
        })
    }
}

impl Clone for FileStream {
    fn clone(&self) -> Self {
        Self {
            file: self.file.clone(),
            len: self.len,
            window_size: self.window_size,
            offset: 0,
            map: None,
        }
    }
}

impl Stream for FileStream {
    fn window_size(&self) -> usize {
        self.window_size
    }
    fn offset(&self) -> u64 {
        self.offset
    }
    fn bytes(&self) -> &[u8] {
        self.map.as_deref().unwrap_or(&[])
    }
    fn is_eof(&self) -> bool {
        self.offset + self.bytes().len() as u64 == self.len
    }
    fn load(&mut self, offset: u64) -> io::Result<()> {
        if offset > self.len {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        // Drop first: replacing a window must never keep two windows resident.
        self.map = None;
        self.offset = offset;
        let len = (self.len - offset).min(self.window_size as u64) as usize;
        if len != 0 {
            // SAFETY: open's caller guarantees file immutability for all clones.
            self.map = Some(unsafe {
                MmapOptions::new()
                    .offset(offset)
                    .len(len)
                    .map(&*self.file)?
            });
        }
        Ok(())
    }
}

impl SeekableStream for FileStream {
    fn len(&self) -> u64 {
        self.len
    }
}
