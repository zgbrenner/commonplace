//! Text reading with encoding detection. Files are not assumed to be UTF-8;
//! detection uses BOMs first, then chardetng statistics, decoding through
//! encoding_rs with replacement for malformed sequences (reported as a
//! warning, never a crash).

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Decoded text plus how it was decoded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecodedText {
    pub content: String,
    /// Encoding label actually used (e.g. "UTF-8", "windows-1252").
    pub encoding: String,
    /// True when malformed sequences were replaced during decoding.
    pub had_replacements: bool,
    /// True when the file exceeded `max_bytes` and was truncated.
    pub truncated: bool,
}

/// Read a file as text, capped at `max_bytes`.
pub fn read_text(path: &Path, max_bytes: usize) -> std::io::Result<DecodedText> {
    let raw = std::fs::read(path)?;
    let truncated = raw.len() > max_bytes;
    let slice = if truncated {
        &raw[..max_bytes]
    } else {
        &raw[..]
    };

    // BOM sniffing first; otherwise statistical detection.
    let encoding = match encoding_rs::Encoding::for_bom(slice) {
        Some((enc, _)) => enc,
        None => {
            // Desktop file reading, not web content: allowing UTF-8 and
            // ISO-2022-JP detection is the correct posture here.
            let mut detector =
                chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Allow);
            detector.feed(slice, true);
            detector.guess(None, chardetng::Utf8Detection::Allow)
        }
    };
    let (content, actual, had_replacements) = encoding.decode(slice);
    Ok(DecodedText {
        content: content.into_owned(),
        encoding: actual.name().to_string(),
        had_replacements,
        truncated,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn reads_utf8() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("a.txt");
        std::fs::write(&f, "héllo wörld ☃").unwrap();
        let t = read_text(&f, 1024).unwrap();
        assert_eq!(t.content, "héllo wörld ☃");
        assert_eq!(t.encoding, "UTF-8");
        assert!(!t.had_replacements);
    }

    #[test]
    fn detects_windows_1252() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("legacy.txt");
        // "café" in windows-1252: é = 0xE9
        std::fs::write(&f, b"caf\xe9 latte").unwrap();
        let t = read_text(&f, 1024).unwrap();
        assert!(t.content.contains("café"), "got: {}", t.content);
    }

    #[test]
    fn truncates_large_files() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("big.txt");
        std::fs::write(&f, "a".repeat(100)).unwrap();
        let t = read_text(&f, 10).unwrap();
        assert!(t.truncated);
        assert_eq!(t.content.len(), 10);
    }

    #[test]
    fn utf16_bom_handled() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("u16.txt");
        let mut bytes = vec![0xFF, 0xFE]; // UTF-16LE BOM
        for unit in "hi".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        std::fs::write(&f, &bytes).unwrap();
        let t = read_text(&f, 1024).unwrap();
        assert_eq!(t.content, "hi");
        assert_eq!(t.encoding, "UTF-16LE");
    }
}
