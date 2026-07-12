//! Token budget utilities.
//!
//! All token counting uses the same approximation: 1 token ~ 4 UTF-8 bytes.

/// Approximate token count for a string.
#[inline]
pub fn approx(byte_len: usize) -> usize {
    byte_len.div_ceil(4)
}

/// Approximate token count for a serialised record (adds one for trailing newline).
#[inline]
pub fn approx_line(byte_len: usize) -> usize {
    (byte_len + 4) / 4
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_string() {
        assert_eq!(approx(0), 0);
    }

    #[test]
    fn four_bytes_is_one_token() {
        assert_eq!(approx(4), 1);
    }

    #[test]
    fn rounds_up() {
        assert_eq!(approx(1), 1);
        assert_eq!(approx(5), 2);
    }
}
