//! Executable Perpl prices at a clip size, from one `perpl-sdk` snapshot.

use alloy::{eips::BlockId, providers::Provider};
use anyhow::Context;
use perpl_sdk::{Chain, num::Converter, state::SnapshotBuilder};
use serde::Serialize;

use crate::stats::decimal;

#[derive(Debug, Serialize, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Fill {
    /// The book fills the whole clip.
    Filled {
        vwap: f64,
        worst_price: f64,
        filled_usd: f64,
        cost_vs_touch_bps: f64,
    },
    /// The book holds less than the clip on this side.
    Short {
        filled_usd: f64,
    },
    EmptySide,
}

#[derive(Debug, Serialize)]
pub struct Clip {
    pub clip_usd: u64,
    /// Selling into the bids.
    pub sell: Fill,
    /// Buying from the asks.
    pub buy: Fill,
}

#[derive(Debug, Serialize)]
pub struct Depth {
    pub perp_id: u32,
    pub block: u64,
    pub touch_bid: Option<f64>,
    pub touch_ask: Option<f64>,
    pub clips: Vec<Clip>,
}

/// `impact` is the SDK's (worst price, size, size-weighted price, notional);
/// the cost is how far the size-weighted price is from the touch, against
/// the taker.
fn fill(
    impact: Option<(f64, f64, f64, f64)>,
    touch: Option<f64>,
    clip_usd: u64,
    sell: bool,
) -> Fill {
    match (impact, touch) {
        (None, _) | (_, None) => Fill::EmptySide,
        (Some((_, _, _, filled_usd)), Some(_)) if filled_usd < clip_usd as f64 => {
            Fill::Short { filled_usd }
        }
        (Some((worst_price, _, vwap, filled_usd)), Some(touch)) => {
            let away = if sell { touch - vwap } else { vwap - touch };
            Fill::Filled {
                vwap,
                worst_price,
                filled_usd,
                cost_vs_touch_bps: away / touch * 1e4,
            }
        }
    }
}

pub async fn measure<P: Provider + Clone>(
    provider: &P,
    chain: &Chain,
    perp_id: u32,
    clips_usd: &[u64],
) -> anyhow::Result<Depth> {
    let block = provider
        .get_block_number()
        .await
        .context("reading the latest block")?;
    let exchange = SnapshotBuilder::new(
        &chain.clone().with_perpetuals(vec![perp_id]),
        provider.clone(),
    )
    .at_block(BlockId::number(block))
    .build()
    .await
    .with_context(|| format!("snapshot at block {block}"))?;
    let book = exchange
        .perpetuals()
        .get(&perp_id)
        .with_context(|| format!("perp {perp_id} is not in the snapshot"))?
        .l3_book();

    let touch_bid = book.best_bid().map(|(p, _)| decimal(&p)).transpose()?;
    let touch_ask = book.best_ask().map(|(p, _)| decimal(&p)).transpose()?;
    let to_f64 = |i: Option<(_, _, _, _)>| -> anyhow::Result<Option<(f64, f64, f64, f64)>> {
        i.map(|(worst, size, vwap, notional)| {
            Ok((
                decimal(&worst)?,
                decimal(&size)?,
                decimal(&vwap)?,
                decimal(&notional)?,
            ))
        })
        .transpose()
    };
    let clips = clips_usd
        .iter()
        .map(|&clip_usd| {
            let want = Converter::new(0).from_u64(clip_usd);
            Ok(Clip {
                clip_usd,
                sell: fill(
                    to_f64(book.bid_impact_notional(want))?,
                    touch_bid,
                    clip_usd,
                    true,
                ),
                buy: fill(
                    to_f64(book.ask_impact_notional(want))?,
                    touch_ask,
                    clip_usd,
                    false,
                ),
            })
        })
        .collect::<anyhow::Result<_>>()?;
    Ok(Depth {
        perp_id,
        block,
        touch_bid,
        touch_ask,
        clips,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_book_is_not_a_price() {
        assert_eq!(
            fill(Some((1.0, 10.0, 1.0, 50.0)), Some(1.0), 100, true),
            Fill::Short { filled_usd: 50.0 }
        );
        assert_eq!(fill(None, Some(1.0), 100, true), Fill::EmptySide);
    }

    #[test]
    fn cost_is_against_the_taker_on_both_sides() {
        let Fill::Filled {
            cost_vs_touch_bps: sell,
            ..
        } = fill(Some((0.98, 1.0, 0.99, 100.0)), Some(1.0), 100, true)
        else {
            panic!("filled");
        };
        let Fill::Filled {
            cost_vs_touch_bps: buy,
            ..
        } = fill(Some((1.02, 1.0, 1.01, 100.0)), Some(1.0), 100, false)
        else {
            panic!("filled");
        };
        assert!(sell > 0.0 && buy > 0.0);
    }
}
