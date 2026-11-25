/// Finds the end of an HTML head element without allocating a lowercase copy.
///
/// The scan is ASCII case-insensitive and returns the byte offset immediately
/// after the first closing `</head>` tag.
pub fn closing_head_end(bytes: &[u8]) -> Option<usize> {
    const NEEDLE: &[u8] = b"</head>";
    bytes
        .windows(NEEDLE.len())
        .position(|window| {
            window
                .iter()
                .zip(NEEDLE)
                .all(|(actual, expected)| actual.eq_ignore_ascii_case(expected))
        })
        .map(|start| start + NEEDLE.len())
}

#[cfg(test)]
mod tests {
    use super::closing_head_end;

    #[test]
    fn closing_head_detection_is_case_insensitive() {
        assert_eq!(closing_head_end(b"<HTML><HEAD></HEAD><body>"), Some(19));
        assert_eq!(closing_head_end(b"</HeAd>"), Some(7));
        assert_eq!(closing_head_end(b"<head><title>x</title>"), None);
    }

    #[test]
    fn finds_first_closing_head_tag() {
        assert_eq!(closing_head_end(b"</head>middle</head>"), Some(7));
    }
}
