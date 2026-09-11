use s_parser::{NtTokenizer, QuadReader, SliceStream};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    time::Instant,
};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("file")?;
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    let size = 8 * 1024 * 1024;
    let mut samples = Vec::new();
    for offset in [0, length / 2, length - size as u64] {
        file.seek(SeekFrom::Start(offset))?;
        let mut bytes = vec![0; size];
        file.read_exact(&mut bytes)?;
        let start = if offset == 0 {
            0
        } else {
            bytes.iter().position(|b| *b == b'\n').ok_or("no newline")? + 1
        };
        let end = bytes
            .iter()
            .rposition(|b| *b == b'\n')
            .ok_or("no newline")?
            + 1;
        samples.push((bytes, start, end));
    }
    for (sample, (bytes, begin, end)) in samples.iter().enumerate() {
        let bytes = &bytes[*begin..*end];
        let start = Instant::now();
        let mut count = 0u64;
        for _ in 0..32 {
            let mut reader = QuadReader::new(SliceStream::new(bytes, size)?, NtTokenizer);
            while let Some(q) = reader.next()? {
                std::hint::black_box(q.tokens());
                count += 1;
            }
        }
        let seconds = start.elapsed().as_secs_f64();
        println!(
            "sample={sample} bytes={} iterations=32 triples={count} seconds={seconds:.6} MiB_s={:.1}",
            bytes.len(),
            bytes.len() as f64 * 32.0 / 1048576.0 / seconds
        );
    }
    Ok(())
}
