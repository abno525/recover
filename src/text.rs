//! Small text helpers for output handling (ANSI stripping, lossy decoding).

/// Remove ANSI escape sequences (colors, cursor moves) from raw bytes.
pub fn strip_ansi(bytes: &[u8]) -> Vec<u8> {
    strip_ansi_escapes::strip(bytes)
}

/// Decode bytes to a String after stripping ANSI escapes and normalizing CRLF line
/// endings to LF. Storage stays raw; this is for matching and display only.
pub fn decode_lossy_stripped(bytes: &[u8]) -> String {
    String::from_utf8_lossy(&strip_ansi(bytes))
        .replace("\r\n", "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_ansi_and_normalizes_crlf() {
        let raw = b"\x1b[31mline1\x1b[0m\r\nline2\r\n";
        assert_eq!(decode_lossy_stripped(raw), "line1\nline2\n");
    }

    #[test]
    fn keeps_bytes_when_plain() {
        assert_eq!(decode_lossy_stripped(b"just text"), "just text");
    }
}
