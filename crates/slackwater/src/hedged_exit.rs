//! A holder who hedges on Perpl and later sells through the atomic exit, net
//! of funding: the hedge opens at each sample and exits at the next sample
//! whose taker exit edge is at least 0, against
//! waiting unhedged and selling at the Kuru bid at that exit sample. The Kuru
//! bid cancels, and so does the price move against the Perpl oracle; what
//! remains is the hedge price's premium to the oracle at arrival, less the
//! Perpl ask's premium to the oracle at exit, less fees.

use serde::Serialize;

use crate::{
    books::{self, Sample, Series},
    funding::Intervals,
    stats::{self, Summary},
};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Arrival {
    pub taker_hedge_bps: f64,
    pub maker_hedge_bps: f64,
    pub funding_to_short_bps: f64,
    pub intervals: usize,
}

/// One entry per valid sample that has a later exit. The taker hedge shorts
/// at the Perpl bid and pays the taker fee; the maker hedge is assumed filled
/// at the Perpl ask and pays `maker_hedge_fee_bps`. Both close at the Perpl
/// ask at the exit sample and pay the taker fee there.
pub fn arrivals(valid: &[Sample], intervals: &Intervals, maker_hedge_fee_bps: f64) -> Vec<Arrival> {
    let next = books::next_exit(valid);
    valid
        .iter()
        .zip(next)
        .filter_map(|(a, x)| x.map(|x| (a, &valid[x])))
        .map(|(a, x)| {
            let close = x.perpl_ask / x.oracle;
            let (funding_to_short_bps, intervals) = intervals.short_receives_bps(a.block, x.block);
            Arrival {
                taker_hedge_bps: (a.perpl_bid / a.oracle - close) * 1e4
                    - x.taker_fee_bps
                    - a.taker_fee_bps,
                maker_hedge_bps: (a.perpl_ask / a.oracle - close) * 1e4
                    - x.taker_fee_bps
                    - maker_hedge_fee_bps,
                funding_to_short_bps,
                intervals,
            }
        })
        .collect()
}

#[derive(Debug, Serialize)]
pub struct MarketReport {
    pub perp_id: u32,
    pub market: String,
    pub from_block: Option<u64>,
    pub to_block: Option<u64>,
    pub samples: usize,
    pub valid_samples: usize,
    pub skipped_readings: usize,
    pub arrivals: usize,
    pub exited: usize,
    pub outcome: &'static str,
    pub overwritten_intervals: usize,
    pub gross_taker_hedge_bps: Summary,
    pub gross_maker_hedge_bps: Summary,
    pub funding_to_short_bps: Summary,
    pub intervals_held: Summary,
    pub net_taker_hedge_bps: Summary,
    pub net_maker_hedge_bps: Summary,
}

pub fn report(
    perp_id: u32,
    market: &str,
    series: &Series,
    intervals: &Intervals,
    maker_hedge_fee_bps: f64,
) -> MarketReport {
    let valid: Vec<Sample> = series
        .samples
        .iter()
        .filter(|s| s.valid_perpl() && s.valid_kuru())
        .copied()
        .collect();
    let arrivals = arrivals(&valid, intervals, maker_hedge_fee_bps);
    let column =
        |f: fn(&Arrival) -> f64| stats::summary(&arrivals.iter().map(f).collect::<Vec<_>>());
    MarketReport {
        perp_id,
        market: market.to_owned(),
        from_block: valid.first().map(|s| s.block),
        to_block: valid.last().map(|s| s.block),
        samples: series.samples.len(),
        valid_samples: valid.len(),
        skipped_readings: series.skipped.len(),
        arrivals: valid.len(),
        exited: arrivals.len(),
        outcome: if arrivals.is_empty() {
            "no exits"
        } else {
            "exits"
        },
        overwritten_intervals: intervals.overwritten,
        gross_taker_hedge_bps: column(|a| a.taker_hedge_bps),
        gross_maker_hedge_bps: column(|a| a.maker_hedge_bps),
        funding_to_short_bps: column(|a| a.funding_to_short_bps),
        intervals_held: column(|a| a.intervals as f64),
        net_taker_hedge_bps: column(|a| a.taker_hedge_bps + a.funding_to_short_bps),
        net_maker_hedge_bps: column(|a| a.maker_hedge_bps + a.funding_to_short_bps),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::funding::{Event, intervals};

    fn sample(block: u64, kuru_bid: f64) -> Sample {
        Sample {
            block,
            perpl_bid: 99.0,
            perpl_ask: 100.0,
            oracle: 100.0,
            kuru_bid: Some(kuru_bid),
            kuru_ask: Some(kuru_bid + 1.0),
            taker_fee_bps: 1.0,
        }
    }

    #[test]
    fn funding_is_added_over_the_hold_and_signs_hold() {
        // Arrival at 10 exits at 30 (the first later sample with an exit edge >= 0).
        let valid = [sample(10, 90.0), sample(20, 90.0), sample(30, 101.0)];
        let events = [Event {
            emit_block: 1,
            log_index: 0,
            perp_id: 10,
            event_block: 25,
            rate_pct100k: 4,
        }];
        let iv = &intervals(&events)[&10];
        let out = arrivals(&valid, iv, 0.45);
        assert_eq!(out.len(), 2);
        // Bid 99 against ask 100 at the same oracle: -100 bps, less two taker fees.
        assert!((out[0].taker_hedge_bps - (-102.0)).abs() < 1e-9);
        assert!((out[0].maker_hedge_bps - (-1.45)).abs() < 1e-9);
        assert_eq!(out[0].intervals, 1);
        assert!(out[0].funding_to_short_bps > 0.0);
    }

    #[test]
    fn no_exit_is_reported_as_no_exits() {
        let series = Series {
            samples: vec![sample(10, 90.0), sample(20, 90.0)],
            skipped: vec![],
        };
        let r = report(1, "BTC", &series, &Intervals::default(), 0.45);
        assert_eq!(
            (r.outcome, r.exited, r.net_taker_hedge_bps.p50),
            ("no exits", 0, None)
        );
    }
}
