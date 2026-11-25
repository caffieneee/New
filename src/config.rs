use std::{error::Error, fs, path::Path, time::Duration};

use serde::Deserialize;
use tracing::warn;

pub const MIN_TARGET_INTERVAL_MS: u64 = 5_000;
pub const MIN_GLOBAL_SPACING_MS: u64 = 1_000;
pub const MAX_TARGETS: usize = 1_000;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub interval_ms: u64,
    pub global_min_spacing_ms: u64,
    pub concurrency: usize,
    pub timeout_secs: u64,
    pub max_body_bytes: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            interval_ms: 15_000,
            global_min_spacing_ms: MIN_GLOBAL_SPACING_MS,
            concurrency: 2,
            timeout_secs: 8,
            max_body_bytes: 256 * 1024,
        }
    }
}

impl Config {
    pub fn load() -> Result<Self, Box<dyn Error>> {
        if !Path::new("config.toml").exists() {
            warn!("config.toml not found; using conservative built-in defaults");
            return Ok(Self::default());
        }
        let raw = fs::read_to_string("config.toml")?;
        let mut config: Self = toml::from_str(&raw)?;
        config.interval_ms = config.interval_ms.max(MIN_TARGET_INTERVAL_MS);
        config.global_min_spacing_ms = config.global_min_spacing_ms.max(MIN_GLOBAL_SPACING_MS);
        config.concurrency = config.concurrency.clamp(1, 4);
        config.timeout_secs = config.timeout_secs.clamp(2, 30);
        config.max_body_bytes = config.max_body_bytes.clamp(32 * 1024, 2 * 1024 * 1024);
        Ok(config)
    }

    pub fn interval(&self) -> Duration {
        Duration::from_millis(self.interval_ms)
    }

    pub fn global_spacing(&self) -> Duration {
        Duration::from_millis(self.global_min_spacing_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_respect_safety_floors() {
        let config = Config::default();
        assert!(config.interval_ms >= MIN_TARGET_INTERVAL_MS);
        assert!(config.global_min_spacing_ms >= MIN_GLOBAL_SPACING_MS);
        assert!((1..=4).contains(&config.concurrency));
    }

    #[test]
    fn deserialization_uses_defaults_for_missing_fields() {
        let config: Config = toml::from_str("interval_ms = 900").unwrap();
        assert_eq!(config.global_min_spacing_ms, MIN_GLOBAL_SPACING_MS);
        assert_eq!(config.interval_ms, 900);
    }
}
