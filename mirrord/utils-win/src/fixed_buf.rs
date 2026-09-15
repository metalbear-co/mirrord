//! A fixed-capacity, allocation-free [`core::fmt::Write`] sink.
//!
//! The crash handler must format text in a process that may be out of heap or running on a tiny
//! reserved stack, so it cannot use `String` or anything that allocates. [`FixedBuf`] wraps a
//! caller-owned byte slice: writes past the end are dropped, so formatting never allocates and
//! never panics.

/// A fixed-capacity buffer that implements [`core::fmt::Write`].
///
/// Writes past capacity are dropped, so formatting never allocates and never panics. That is what
/// the crash handler needs.
pub struct FixedBuf<'a> {
    buf: &'a mut [u8],
    len: usize,
}

impl<'a> FixedBuf<'a> {
    /// Wraps a byte slice as an empty buffer.
    pub fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, len: 0 }
    }

    /// The bytes written so far.
    pub fn filled(&self) -> &[u8] {
        self.buf.get(..self.len).unwrap_or(&[])
    }
}

impl core::fmt::Write for FixedBuf<'_> {
    fn write_str(&mut self, value: &str) -> core::fmt::Result {
        let bytes = value.as_bytes();
        let remaining = self.buf.len().saturating_sub(self.len);
        let take = remaining.min(bytes.len());

        if let (Some(destination), Some(source)) = (
            self.buf.get_mut(self.len..self.len + take),
            bytes.get(..take),
        ) {
            destination.copy_from_slice(source);
            self.len += take;
        }

        if take < bytes.len() {
            Err(core::fmt::Error)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use core::fmt::Write as _;

    use super::*;

    #[test]
    fn fixed_buf_write_within_capacity() {
        let mut storage = [0u8; 16];
        let mut buf = FixedBuf::new(&mut storage);
        assert!(write!(buf, "ab{}", 12).is_ok());
        assert_eq!(buf.filled(), b"ab12");
    }

    #[test]
    fn fixed_buf_exact_fill() {
        let mut storage = [0u8; 3];
        let mut buf = FixedBuf::new(&mut storage);
        assert!(write!(buf, "abc").is_ok());
        assert_eq!(buf.filled(), b"abc");
    }

    #[test]
    fn fixed_buf_overflow_truncates_without_panicking() {
        let mut storage = [0u8; 3];
        let mut buf = FixedBuf::new(&mut storage);
        // The overflowing write reports an error, but the prefix is kept and nothing panics.
        assert!(write!(buf, "abcdef").is_err());
        assert_eq!(buf.filled(), b"abc");
    }
}
