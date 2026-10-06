//! Funding per interval from `FundingEventCompleted`, in bps of the funding
//! price, with the sign of a short: a positive rate pays shorts.

use std::collections::BTreeMap;

use alloy::primitives::U256;
use anyhow::Context;

use crate::rows::{self, FundingRow};

/// `actualRatePct100k` is in units of 1e-5 of the funding price per
/// interval, so one unit is 0.1 bps.
const BPS_PER_RATE_UNIT: f64 = 0.1;

#[derive(Debug, Clone, Copy)]
pub struct Event {
    pub emit_block: u64,
    pub log_index: u64,
    pub perp_id: u32,
    pub event_block: u64,
    pub rate_pct100k: i64,
}

/// The last event of each interval of one perp, keyed by event block.
#[derive(Debug, Default)]
pub struct Intervals {
    pub rate_bps: BTreeMap<u64, f64>,
    /// Events beyond the first for the same interval; the last one counts.
    pub overwritten: usize,
}

impl Intervals {
    /// Funding received by a short held at every event block in
    /// `(from_block, to_block]`, and the number of those intervals.
    pub fn short_receives_bps(&self, from_block: u64, to_block: u64) -> (f64, usize) {
        if to_block <= from_block {
            return (0.0, 0);
        }
        self.rate_bps
            .range(from_block + 1..=to_block)
            .fold((0.0, 0), |(sum, n), (_, bps)| (sum + bps, n + 1))
    }
}

/// Groups events per perp; within an interval, the last by emitting block
/// and log index counts.
pub fn intervals(events: &[Event]) -> BTreeMap<u32, Intervals> {
    let mut sorted = events.to_vec();
    sorted.sort_by_key(|e| (e.emit_block, e.log_index));
    let mut out: BTreeMap<u32, Intervals> = BTreeMap::new();
    for e in sorted {
        let entry = out.entry(e.perp_id).or_default();
        let bps = e.rate_pct100k as f64 * BPS_PER_RATE_UNIT;
        if entry.rate_bps.insert(e.event_block, bps).is_some() {
            entry.overwritten += 1;
        }
    }
    out
}

/// An error row stops the decoding: a missing interval would read as an
/// interval with no funding.
pub fn decode(rows: &[FundingRow]) -> anyhow::Result<Vec<Event>> {
    rows.iter()
        .map(|row| {
            let event = rows::decode_funding(row)?;
            let emit_block = row.block.context("a funding event row has no block")?;
            let small = |v: U256, what: &str| -> anyhow::Result<u64> {
                u64::try_from(v).with_context(|| format!("block {emit_block}: {what} out of range"))
            };
            Ok(Event {
                emit_block,
                log_index: row
                    .log_index
                    .with_context(|| format!("block {emit_block}: no log index"))?,
                perp_id: u32::try_from(small(event.perpId, "perp id")?)?,
                event_block: small(event.fundingEventBlock, "event block")?,
                rate_pct100k: event.actualRatePct100k.as_i64(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(emit_block: u64, log_index: u64, event_block: u64, rate_pct100k: i64) -> Event {
        Event {
            emit_block,
            log_index,
            perp_id: 10,
            event_block,
            rate_pct100k,
        }
    }

    #[test]
    fn last_event_of_an_interval_counts() {
        let iv = &intervals(&[ev(100, 2, 120, 5), ev(101, 1, 120, -3), ev(99, 9, 120, 7)])[&10];
        assert_eq!(iv.rate_bps[&120], -3.0 * BPS_PER_RATE_UNIT);
        assert_eq!(iv.overwritten, 2);
    }

    #[test]
    fn boundary_is_open_at_arrival_and_closed_at_exit() {
        let iv = &intervals(&[ev(1, 0, 100, 1), ev(1, 1, 200, 10), ev(1, 2, 300, 100)])[&10];
        let (bps, n) = iv.short_receives_bps(100, 200);
        assert_eq!(n, 1);
        assert!((bps - 1.0).abs() < 1e-12);
        assert_eq!(iv.short_receives_bps(200, 200), (0.0, 0));
    }

    #[test]
    fn positive_rate_is_received_by_a_short() {
        let iv = &intervals(&[ev(1, 0, 50, 4)])[&10];
        assert!(iv.short_receives_bps(0, 100).0 > 0.0);
    }
}
