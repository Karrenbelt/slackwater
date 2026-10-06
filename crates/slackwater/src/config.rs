use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub rpc_url: String,
    pub paths: Paths,
    pub rpc: Rpc,
    pub markets: Vec<Market>,
    pub hedged_exit: HedgedExit,
    pub fills: Fills,
    pub depth: Depth,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Paths {
    pub raw: PathBuf,
    pub derived: PathBuf,
    pub manifest: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rpc {
    pub requests_per_second: u32,
    pub in_flight: usize,
    pub max_rate_limit_retries: u32,
    pub initial_backoff_ms: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Market {
    pub perp_id: u32,
    pub label: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HedgedExit {
    pub maker_hedge_fee_bps: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fills {
    pub perp_id: u32,
    pub max_unmatched_share: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Depth {
    pub perp_id: u32,
    pub clip_usd: Vec<u64>,
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading configuration {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("parsing configuration {}", path.display()))
    }

    fn parse(text: &str) -> anyhow::Result<Self> {
        Ok(toml::from_str(text)?)
    }
}

#[cfg(test)]
mod tests {
    use super::Config;

    const SHIPPED: &str = include_str!("../../../config/data.toml");

    #[test]
    fn shipped_configuration_parses() {
        let config = Config::parse(SHIPPED).expect("config/data.toml parses");
        assert_eq!(config.markets.len(), 3);
    }

    #[test]
    fn missing_field_is_named() {
        let text = SHIPPED.replace("maker_hedge_fee_bps = 0.45", "");
        let err = format!("{:#}", Config::parse(&text).unwrap_err());
        assert!(err.contains("maker_hedge_fee_bps"), "{err}");
    }

    #[test]
    fn malformed_field_is_named() {
        let text = SHIPPED.replace("requests_per_second = 40", "requests_per_second = \"x\"");
        let err = format!("{:#}", Config::parse(&text).unwrap_err());
        assert!(err.contains("requests_per_second"), "{err}");
    }
}
