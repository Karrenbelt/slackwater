//! Same-block Perpl and Kuru readings per market, and the taker exit edge at
//! the touch.

use std::collections::{BTreeMap, HashMap};

use alloy::primitives::U256;
use anyhow::Context;

use crate::rows::{self, BookRow, FeeRow, Source};

/// Kuru `bestBidAsk` returns dollars per unit of base, times 1e18.
const KURU_PRICE_SCALE: f64 = 1e18;
const PPM_PER_BPS: f64 = 100.0;

/// One market at one sample block. Prices are in dollars, fees in bps.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub block: u64,
    pub perpl_bid: f64,
    pub perpl_ask: f64,
    pub oracle: f64,
    pub kuru_bid: Option<f64>,
    pub kuru_ask: Option<f64>,
    pub taker_fee_bps: f64,
}

impl Sample {
    pub fn valid_perpl(&self) -> bool {
        self.perpl_bid > 0.0 && self.perpl_ask > self.perpl_bid
    }

    pub fn valid_kuru(&self) -> bool {
        matches!((self.kuru_bid, self.kuru_ask), (Some(b), Some(a)) if b > 0.0 && a > b)
    }

    /// Exit through the atomic exit at the touch: sell at the Kuru bid, buy
    /// the Perpl ask, less the Perpl taker fee. `None` without a Kuru bid.
    pub fn taker_exit_bps(&self) -> Option<f64> {
        self.kuru_bid
            .map(|kb| (kb - self.perpl_ask) / self.perpl_ask * 1e4 - self.taker_fee_bps)
    }
}

fn to_f64(x: U256) -> f64 {
    f64::from(x)
}

/// Taker fee per perp in bps, from `getTakerFee` rows in ppm.
pub fn taker_fees_bps(rows: &[FeeRow]) -> anyhow::Result<HashMap<u32, f64>> {
    rows.iter()
        .filter(|r| r.function == "getTakerFee")
        .map(|r| Ok((r.perp_id, to_f64(rows::decode_fee(r)?) / PPM_PER_BPS)))
        .collect()
}

/// The samples of one perp, sorted by block, and the readings that were
/// skipped because they were error rows or did not decode.
#[derive(Debug, Default)]
pub struct Series {
    pub samples: Vec<Sample>,
    pub skipped: Vec<String>,
}

/// Samples per perp, keeping only the blocks where both the Perpl and the
/// Kuru reading decode.
pub fn samples(
    rows: &[BookRow],
    fees_bps: &HashMap<u32, f64>,
    perp_ids: &[u32],
) -> anyhow::Result<BTreeMap<u32, Series>> {
    let mut by_key: HashMap<(u64, Source, u32), &BookRow> = HashMap::new();
    for row in rows {
        if let Some(perp) = row.perp_id {
            by_key.insert((row.block, row.source, perp), row);
        }
    }
    let mut blocks: Vec<u64> = rows.iter().map(|r| r.block).collect();
    blocks.sort_unstable();
    blocks.dedup();

    let mut out = BTreeMap::new();
    for &perp in perp_ids {
        let fee = *fees_bps
            .get(&perp)
            .with_context(|| format!("no taker fee for perp {perp}"))?;
        let mut series = Series::default();
        for &block in &blocks {
            let (Some(p), Some(k)) = (
                by_key.get(&(block, Source::Perpl, perp)),
                by_key.get(&(block, Source::Kuru, perp)),
            ) else {
                series
                    .skipped
                    .push(format!("block {block}: no Perpl or no Kuru reading"));
                continue;
            };
            let (info, touch) = match (rows::decode_perpl(p), rows::decode_kuru(k)) {
                (Ok(info), Ok(touch)) => (info, touch),
                (Err(e), _) | (_, Err(e)) => {
                    series.skipped.push(e.to_string());
                    continue;
                }
            };
            let scale = 10f64.powi(
                i32::try_from(info.priceDecimals)
                    .with_context(|| format!("block {block}: price decimals out of range"))?,
            );
            series.samples.push(Sample {
                block,
                perpl_bid: to_f64(info.maxBidPriceONS) / scale,
                perpl_ask: to_f64(info.minAskPriceONS) / scale,
                oracle: to_f64(info.oraclePNS) / scale,
                kuru_bid: touch.bid.map(|v| to_f64(v) / KURU_PRICE_SCALE),
                kuru_ask: touch.ask.map(|v| to_f64(v) / KURU_PRICE_SCALE),
                taker_fee_bps: fee,
            });
        }
        out.insert(perp, series);
    }
    Ok(out)
}

/// For each index, the first later index whose taker exit edge is at least 0.
pub fn next_exit(samples: &[Sample]) -> Vec<Option<usize>> {
    let mut next = None;
    let mut out = vec![None; samples.len()];
    for i in (0..samples.len()).rev() {
        out[i] = next;
        if samples[i].taker_exit_bps().is_some_and(|e| e >= 0.0) {
            next = Some(i);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(block: u64, kuru_bid: f64, perpl_ask: f64) -> Sample {
        Sample {
            block,
            perpl_bid: perpl_ask - 1.0,
            perpl_ask,
            oracle: perpl_ask,
            kuru_bid: Some(kuru_bid),
            kuru_ask: Some(kuru_bid + 1.0),
            taker_fee_bps: 0.0,
        }
    }

    #[test]
    fn next_exit_is_strictly_later() {
        // Exit edges: +, -, +, -.
        let s = [
            sample(1, 101.0, 100.0),
            sample(2, 99.0, 100.0),
            sample(3, 101.0, 100.0),
            sample(4, 99.0, 100.0),
        ];
        assert_eq!(next_exit(&s), vec![Some(2), Some(2), None, None]);
    }

    #[test]
    fn kuru_without_a_side_is_not_valid() {
        let mut s = sample(1, 100.0, 100.0);
        s.kuru_ask = None;
        assert!(!s.valid_kuru());
    }
}
