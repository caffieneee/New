use std::{collections::HashSet, error::Error, fs};

use url::Url;

use crate::config::MAX_TARGETS;

#[derive(Debug, Clone)]
pub struct Target {
    pub code: String,
    pub url: Url,
}

pub fn load_targets() -> Result<Vec<Target>, Box<dyn Error>> {
    let raw = fs::read_to_string("urls.txt")
        .map_err(|_| "urls.txt not found; copy urls.example.txt to urls.txt")?;
    parse_targets(&raw)
}

pub fn parse_targets(raw: &str) -> Result<Vec<Target>, Box<dyn Error>> {
    let mut seen = HashSet::new();
    let mut targets = Vec::new();

    for (line_index, line) in raw.lines().enumerate() {
        let code = line.trim();
        if code.is_empty() || code.starts_with('#') {
            continue;
        }
        if !(2..=64).contains(&code.len())
            || !code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(format!("invalid invite code on urls.txt line {}", line_index + 1).into());
        }
        if !seen.insert(code.to_owned()) {
            continue;
        }
        let url = Url::parse(&format!("https://discord.com/invite/{code}"))?;
        targets.push(Target { code: code.to_owned(), url });
        if targets.len() > MAX_TARGETS {
            return Err(format!("more than {MAX_TARGETS} targets; reduce urls.txt").into());
        }
    }

    if targets.is_empty() {
        return Err("urls.txt contains no valid targets".into());
    }
    Ok(targets)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_comments_and_deduplicates() {
        let targets = parse_targets("# comment\\n alpha \\nalpha\\nbeta_2\\n").unwrap();
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].code, "alpha");
        assert_eq!(targets[1].code, "beta_2");
    }

    #[test]
    fn rejects_invalid_codes() {
        assert!(parse_targets("bad/code\\n").is_err());
        assert!(parse_targets("x\\n").is_err());
    }

    #[test]
    fn rejects_empty_input() {
        assert!(parse_targets("# nothing here\\n").is_err());
    }
}
