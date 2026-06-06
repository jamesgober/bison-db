//! Positional file I/O, abstracted across platforms.
//!
//! The store reads record bodies at arbitrary offsets while appending new
//! records at the tail. Positional (`pread`/`pwrite`-style) access keeps reads
//! on a shared `&File` free of a mutable seek cursor, which is what lets
//! [`Db::get`](crate::Db::get) take `&self`. Each platform exposes the primitive
//! under a different name, so this module presents one `*_exact_at` API over
//! both. Targets that are neither Unix nor Windows have no positional file
//! primitive in `std`; rather than degrade silently, the crate refuses to build
//! its file store there.

use std::fs::File;
use std::io;

/// Reads exactly `buf.len()` bytes starting at `offset`, or fails.
///
/// On both platforms this is a positional read that does not disturb any seek
/// cursor, so concurrent reads through a shared handle do not race.
#[cfg(unix)]
pub(crate) fn read_exact_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.read_exact_at(buf, offset)
}

/// Writes all of `buf` starting at `offset`, or fails.
#[cfg(unix)]
pub(crate) fn write_all_at(file: &File, buf: &[u8], offset: u64) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.write_all_at(buf, offset)
}

/// Windows positional read, looping over `seek_read` to fill the buffer.
#[cfg(windows)]
pub(crate) fn read_exact_at(file: &File, mut buf: &mut [u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match file.seek_read(buf, offset) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "reached end of file before the buffer was filled",
                ));
            }
            Ok(n) => {
                let advanced = buf;
                buf = &mut advanced[n..];
                offset += n as u64;
            }
            Err(ref e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Windows positional write, looping over `seek_write` to drain the buffer.
#[cfg(windows)]
pub(crate) fn write_all_at(file: &File, mut buf: &[u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match file.seek_write(buf, offset) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "wrote zero bytes before the buffer was drained",
                ));
            }
            Ok(n) => {
                buf = &buf[n..];
                offset += n as u64;
            }
            Err(ref e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
compile_error!(
    "bison-db's file store needs positional file I/O, which std provides only on \
     Unix and Windows targets; disable the `std` feature to use the in-memory \
     document model alone"
);
