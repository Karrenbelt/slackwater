//! MON maker fills classified by the taker's side, through `perpl-sdk`'s
//! trade processor over the logs of each fill's transaction.

use std::collections::{BTreeMap, HashMap, HashSet};

use alloy::{
    primitives::{Address, B256, Bytes, TxHash},
    providers::{DynProvider, Provider},
    sol_types::{SolEvent, SolEventInterface},
};
use anyhow::Context;
use perpl_sdk::{
    abi::dex::Exchange::{
        ExchangeEvents, MakerOrderFilled, MakerOrderFilledV2, OrderBatchCompleted, OrderRequest,
        OrderRequestV2, TakerOrderFilled, TakerOrderFilledV2,
    },
    stream::{NormalizationConfig, TradeProcessor},
    types::{BlockEvents, EventContext, OrderSide, StateInstant},
};
use serde::{Deserialize, Serialize};
use tokio::task::JoinSet;

use crate::{
    rows::MakerFillRow,
    stats::{self, Summary, decimal},
};

/// Events stored from each receipt: what the trade processor reads.
pub const STORED_TOPICS: [B256; 7] = [
    OrderRequest::SIGNATURE_HASH,
    OrderRequestV2::SIGNATURE_HASH,
    MakerOrderFilled::SIGNATURE_HASH,
    MakerOrderFilledV2::SIGNATURE_HASH,
    TakerOrderFilled::SIGNATURE_HASH,
    TakerOrderFilledV2::SIGNATURE_HASH,
    OrderBatchCompleted::SIGNATURE_HASH,
];

/// Events not emitted from exchange contract v1.1.7.4 on; expected to be absent.
const V1_TOPICS: [B256; 3] = [
    OrderRequest::SIGNATURE_HASH,
    MakerOrderFilled::SIGNATURE_HASH,
    TakerOrderFilled::SIGNATURE_HASH,
];

/// One stored log, as the chain returned it, or a transaction whose receipt
/// could not be read.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TxLogRow {
    Log {
        block: u64,
        tx: TxHash,
        tx_index: u64,
        log_index: u64,
        address: Address,
        topics: Vec<B256>,
        data: Bytes,
    },
    Error {
        tx: TxHash,
        error: String,
    },
}

async fn receipt_rows(
    provider: DynProvider,
    exchange: Address,
    tx: TxHash,
) -> anyhow::Result<Vec<TxLogRow>> {
    let error = |error: String| Ok(vec![TxLogRow::Error { tx, error }]);
    let receipt = match provider.get_transaction_receipt(tx).await {
        Ok(Some(receipt)) => receipt,
        Ok(None) => return error("no receipt".to_owned()),
        Err(e) => return error(e.to_string()),
    };
    receipt
        .logs()
        .iter()
        .filter(|l| {
            l.address() == exchange
                && l.topics()
                    .first()
                    .is_some_and(|t| STORED_TOPICS.contains(t))
        })
        .map(|l| {
            let missing = |what: &str| format!("transaction {tx}: a log has no {what}");
            Ok(TxLogRow::Log {
                block: l.block_number.with_context(|| missing("block"))?,
                tx,
                tx_index: l
                    .transaction_index
                    .with_context(|| missing("transaction index"))?,
                log_index: l.log_index.with_context(|| missing("log index"))?,
                address: l.address(),
                topics: l.topics().to_vec(),
                data: l.data().data.clone(),
            })
        })
        .collect()
}

/// Fetches the receipts with at most `in_flight` requests outstanding; the
/// provider's throttle sets the pace.
pub async fn fetch(
    provider: &DynProvider,
    exchange: Address,
    txs: Vec<TxHash>,
    in_flight: usize,
) -> anyhow::Result<Vec<TxLogRow>> {
    let total = txs.len();
    let mut pending = txs.into_iter();
    let mut tasks = JoinSet::new();
    let mut rows = Vec::new();
    let mut done = 0;
    loop {
        while tasks.len() < in_flight {
            let Some(tx) = pending.next() else { break };
            tasks.spawn(receipt_rows(provider.clone(), exchange, tx));
        }
        let Some(joined) = tasks.join_next().await else {
            break;
        };
        rows.extend(joined.context("a receipt task stopped")??);
        done += 1;
        if done % 1000 == 0 || done == total {
            eprintln!("receipts {done} of {total}");
        }
    }
    rows.sort_by_key(|r| match r {
        TxLogRow::Log {
            block,
            tx_index,
            log_index,
            ..
        } => (0, *block, *tx_index, *log_index),
        TxLogRow::Error { .. } => (1, 0, 0, 0),
    });
    Ok(rows)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unmatched {
    NoReceipt,
    NotInTrade,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// The taker bought: the maker's order rested on the ask.
    AskSide,
    BidSide,
    Unmatched(Unmatched),
}

/// A maker fill the trade processor matched to a taker.
#[derive(Debug, Clone, Copy)]
pub struct Matched {
    pub taker_side: OrderSide,
    pub notional_usd: f64,
}

#[derive(Debug, Default)]
pub struct Matches {
    /// Maker fills of the perp, by (transaction, log index).
    pub fills: HashMap<(TxHash, u64), Matched>,
    pub no_receipt: HashSet<TxHash>,
    pub v1_events: usize,
}

/// Runs the trade processor over each transaction's logs on its own, so that
/// no request context carries over from one transaction to the next.
pub fn match_fills(
    rows: &[TxLogRow],
    config: &NormalizationConfig,
    perp_id: u32,
) -> anyhow::Result<Matches> {
    let mut by_tx: BTreeMap<(u64, u64, TxHash), Vec<EventContext<ExchangeEvents>>> =
        BTreeMap::new();
    let mut no_receipt = HashSet::new();
    let mut v1_events = 0;
    for row in rows {
        match row {
            TxLogRow::Log {
                block,
                tx,
                tx_index,
                log_index,
                topics,
                data,
                ..
            } => {
                if topics.first().is_some_and(|t| V1_TOPICS.contains(t)) {
                    v1_events += 1;
                }
                let event = ExchangeEvents::decode_raw_log(topics, data)
                    .with_context(|| format!("transaction {tx} log {log_index} does not decode"))?;
                by_tx
                    .entry((*block, *tx_index, *tx))
                    .or_default()
                    .push(EventContext::new(*tx, *tx_index, *log_index, event));
            }
            TxLogRow::Error { tx, .. } => {
                no_receipt.insert(*tx);
            }
        }
    }

    let mut matched = HashMap::new();
    for ((block, _, tx), mut events) in by_tx {
        events.sort_by_key(|e| e.log_index());
        // The processor reads no timestamp; the block number is what it keeps.
        let block_events = BlockEvents::new(StateInstant::new(block, 0), events);
        let trades = TradeProcessor::new(config.clone()).process_block(&block_events);
        for trade in trades.events().iter().map(|t| t.event()) {
            if trade.perpetual_id != perp_id {
                continue;
            }
            for fill in &trade.maker_fills {
                let notional_usd = decimal(&fill.price)? * decimal(&fill.size)?;
                matched.insert(
                    (tx, fill.log_index),
                    Matched {
                        taker_side: trade.taker_side,
                        notional_usd,
                    },
                );
            }
        }
    }
    Ok(Matches {
        fills: matched,
        no_receipt,
        v1_events,
    })
}

pub fn classify(fill: (TxHash, u64), matches: &Matches) -> Class {
    match (
        matches.fills.get(&fill),
        matches.no_receipt.contains(&fill.0),
    ) {
        (Some(m), _) => match m.taker_side {
            OrderSide::Bid => Class::AskSide,
            OrderSide::Ask => Class::BidSide,
        },
        (None, true) => Class::Unmatched(Unmatched::NoReceipt),
        (None, false) => Class::Unmatched(Unmatched::NotInTrade),
    }
}

/// Block time interpolated linearly between stored timestamps a fixed number
/// of blocks apart. `None` outside the stored range.
pub fn time_of(block_times: &[(u64, u64)], block: u64) -> Option<f64> {
    let (b0, _) = *block_times.first()?;
    let step = block_times.get(1)?.0.checked_sub(b0).filter(|s| *s > 0)?;
    let k = usize::try_from(block.checked_sub(b0)? / step).ok()?;
    let (bk, tk) = *block_times.get(k)?;
    let (_, tk1) = *block_times.get(k + 1)?;
    Some(tk as f64 + (block - bk) as f64 * (tk1 as f64 - tk as f64) / step as f64)
}

#[derive(Debug, Default, Serialize)]
pub struct Counts {
    pub ask_side: usize,
    pub bid_side: usize,
    pub unmatched_no_receipt: usize,
    pub unmatched_not_in_trade: usize,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub perp_id: u32,
    pub from_block: Option<u64>,
    pub to_block: Option<u64>,
    pub fills: usize,
    #[serde(flatten)]
    pub counts: Counts,
    pub unmatched_share: f64,
    pub v1_events_in_receipts: usize,
    pub interpreted: bool,
    pub ask_side_figures: Option<AskSideFigures>,
    pub method: [&'static str; 3],
}

/// Classifies every stored fill; above `max_unmatched_share` of unmatched
/// fills, the ask-side figures are withheld.
pub fn report(
    perp_id: u32,
    stored: &[MakerFillRow],
    block_times: &[(u64, u64)],
    matches: &Matches,
    max_unmatched_share: f64,
) -> anyhow::Result<Report> {
    let mut counts = Counts::default();
    let mut all_times = Vec::with_capacity(stored.len());
    let mut ask_side = Vec::new();
    for f in stored {
        let t = time_of(block_times, f.block)
            .with_context(|| format!("block {} is outside the stored block times", f.block))?;
        all_times.push(t);
        match classify((f.tx, f.log_index), matches) {
            Class::AskSide => {
                counts.ask_side += 1;
                let m = matches
                    .fills
                    .get(&(f.tx, f.log_index))
                    .context("an ask-side fill is matched")?;
                ask_side.push((t, f.tx, m.notional_usd));
            }
            Class::BidSide => counts.bid_side += 1,
            Class::Unmatched(Unmatched::NoReceipt) => counts.unmatched_no_receipt += 1,
            Class::Unmatched(Unmatched::NotInTrade) => counts.unmatched_not_in_trade += 1,
        }
    }
    let unmatched = counts.unmatched_no_receipt + counts.unmatched_not_in_trade;
    let classified = counts.ask_side + counts.bid_side + unmatched;
    anyhow::ensure!(
        classified == stored.len(),
        "classes add up to {classified} of {} fills",
        stored.len()
    );
    let unmatched_share = unmatched as f64 / stored.len() as f64;
    let interpreted = unmatched_share <= max_unmatched_share;
    Ok(Report {
        perp_id,
        from_block: stored.iter().map(|f| f.block).min(),
        to_block: stored.iter().map(|f| f.block).max(),
        fills: stored.len(),
        counts,
        unmatched_share,
        v1_events_in_receipts: matches.v1_events,
        interpreted,
        ask_side_figures: interpreted
            .then(|| ask_side_figures(&all_times, &ask_side))
            .flatten(),
        method: [
            "A maker fill is ask-side when perpl-sdk's TradeProcessor, run over the logs of its transaction alone, attaches it to a taker whose request side is Bid (OpenLong or CloseShort).",
            "Times are interpolated linearly between the block timestamps stored every 1,000 blocks; full hours are those strictly inside the span of all fills.",
            "Above the configured share of unmatched fills, the ask-side figures are withheld.",
        ],
    })
}

#[derive(Debug, Serialize)]
pub struct AskSideFigures {
    pub full_hours: usize,
    pub hourly_usd: Summary,
    pub hourly_fills: Summary,
    pub gap_s_between_fills: Summary,
    pub gap_s_between_transactions: Summary,
}

/// Full hours are those strictly inside the span of `all_times`, the times of
/// every fill; `ask_side` holds (time, transaction, notional) for ask-side fills.
pub fn ask_side_figures(
    all_times: &[f64],
    ask_side: &[(f64, TxHash, f64)],
) -> Option<AskSideFigures> {
    let hour = |t: f64| (t / 3600.0).floor() as i64;
    let first = hour(all_times.iter().copied().reduce(f64::min)?);
    let last = hour(all_times.iter().copied().reduce(f64::max)?);
    let mut by_hour: BTreeMap<i64, (f64, f64)> = BTreeMap::new();
    for &(t, _, usd) in ask_side {
        let e = by_hour.entry(hour(t)).or_default();
        e.0 += usd;
        e.1 += 1.0;
    }
    let hours: Vec<(f64, f64)> = (first + 1..last)
        .map(|h| by_hour.get(&h).copied().unwrap_or((0.0, 0.0)))
        .collect();

    let mut times: Vec<f64> = ask_side.iter().map(|&(t, ..)| t).collect();
    times.sort_by(f64::total_cmp);
    let mut tx_first: HashMap<TxHash, f64> = HashMap::new();
    for &(t, tx, _) in ask_side {
        tx_first
            .entry(tx)
            .and_modify(|v| *v = v.min(t))
            .or_insert(t);
    }
    let mut tx_times: Vec<f64> = tx_first.into_values().collect();
    tx_times.sort_by(f64::total_cmp);
    let gaps = |v: &[f64]| v.windows(2).map(|w| w[1] - w[0]).collect::<Vec<_>>();

    Some(AskSideFigures {
        full_hours: hours.len(),
        hourly_usd: stats::summary(&hours.iter().map(|h| h.0).collect::<Vec<_>>()),
        hourly_fills: stats::summary(&hours.iter().map(|h| h.1).collect::<Vec<_>>()),
        gap_s_between_fills: stats::summary(&gaps(&times)),
        gap_s_between_transactions: stats::summary(&gaps(&tx_times)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolation_matches_stats_sh() {
        let bt = [(1000, 100), (2000, 400), (3000, 500)];
        assert_eq!(time_of(&bt, 1500), Some(250.0));
        assert_eq!(time_of(&bt, 2000), Some(400.0));
        assert_eq!(time_of(&bt, 999), None);
        assert_eq!(time_of(&bt, 3000), None);
    }

    #[test]
    fn a_taker_bid_fills_the_ask() {
        let tx = TxHash::repeat_byte(1);
        let matched = HashMap::from([
            (
                (tx, 1),
                Matched {
                    taker_side: OrderSide::Bid,
                    notional_usd: 1.0,
                },
            ),
            (
                (tx, 2),
                Matched {
                    taker_side: OrderSide::Ask,
                    notional_usd: 1.0,
                },
            ),
        ]);
        let m = Matches {
            fills: matched,
            no_receipt: HashSet::from([TxHash::repeat_byte(2)]),
            v1_events: 0,
        };
        assert_eq!(classify((tx, 1), &m), Class::AskSide);
        assert_eq!(classify((tx, 2), &m), Class::BidSide);
        assert_eq!(
            classify((tx, 3), &m),
            Class::Unmatched(Unmatched::NotInTrade)
        );
        assert_eq!(
            classify((TxHash::repeat_byte(2), 1), &m),
            Class::Unmatched(Unmatched::NoReceipt)
        );
    }

    #[test]
    fn hours_with_no_ask_side_fill_count_as_zero() {
        let tx = TxHash::repeat_byte(1);
        // Fills span hours 0 to 3, so hours 1 and 2 are full; only hour 1 has an ask-side fill.
        let all = [10.0, 3700.0, 3.0 * 3600.0 + 5.0];
        let figures = ask_side_figures(&all, &[(3700.0, tx, 50.0)]).expect("fills exist");
        assert_eq!(figures.full_hours, 2);
        assert_eq!(figures.hourly_usd.max, Some(50.0));
        assert_eq!(figures.hourly_usd.min, Some(0.0));
    }
}
