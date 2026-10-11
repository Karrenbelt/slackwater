//! `config/keeper.toml`.

use std::path::{Path, PathBuf};

use alloy::primitives::Address;
use serde::Deserialize;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("reading {path}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("parsing {path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeeperConfig {
    pub rpc_url: String,
    pub instance: Instance,
    pub record: Record,
    pub clip: Clip,
    pub stop: Stop,
    pub run: Run,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Instance {
    pub address: Address,
    pub keeper: Address,
    pub perp_id: u64,
    pub kuru_book: Address,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub path: PathBuf,
    pub round: u32,
    pub clip: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Clip {
    pub lots: u64,
    pub max_posts: usize,
    pub requote_min: u64,
    pub oracle_age_to_sign_sec: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stop {
    pub top_up_mark_bps: u64,
    pub mark_rise_signal_bps: u64,
    pub funding_signal_bps: u64,
    pub exit_guard_bps: u64,
    pub widened_exit_guard_bps: u64,
    pub signal_repeat_min: u64,
    pub gas_signal_mon: u64,
    pub gas_stop_mon: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Run {
    pub poll_sec: u64,
    pub receipt_timeout_sec: u64,
    pub max_failed_cycles: u32,
}

impl KeeperConfig {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(&text).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })
    }

    fn parse(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }
}

#[cfg(test)]
mod tests {
    use super::KeeperConfig;

    const SHIPPED: &str = include_str!("../../../config/keeper.toml");

    #[test]
    fn shipped_configuration_parses() {
        let config = KeeperConfig::parse(SHIPPED).expect("config/keeper.toml parses");
        assert_eq!(config.clip.lots, 1850);
        assert_eq!(config.record.clip, 2);
    }

    #[test]
    fn missing_field_is_named() {
        let text = SHIPPED.replace("mark_rise_signal_bps = 15000", "");
        let err = KeeperConfig::parse(&text).unwrap_err().to_string();
        assert!(err.contains("mark_rise_signal_bps"), "{err}");
    }

    #[test]
    fn malformed_field_is_named() {
        let text = SHIPPED.replace("requote_min = 15", "requote_min = \"x\"");
        let err = KeeperConfig::parse(&text).unwrap_err().to_string();
        assert!(err.contains("requote_min"), "{err}");
    }
}
