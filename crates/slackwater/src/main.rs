mod bindings;
mod books;
mod config;
mod depth;
mod fills;
mod funding;
mod hedged_exit;
mod manifest;
mod rows;
mod rpc;
mod stats;

use std::path::{Path, PathBuf};

use anyhow::Context;
use clap::{Parser, Subcommand};
use perpl_sdk::{Chain, stream::NormalizationConfig};
use serde::Serialize;

use crate::config::Config;

#[derive(Parser)]
#[command(about = "Reads of Perpl and Kuru, and the checks over them")]
struct Cli {
    #[arg(long, default_value = "config/data.toml")]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Reads the chain id, to confirm the RPC and the configuration.
    Ping,
    #[command(subcommand)]
    Fetch(Fetch),
    #[command(subcommand)]
    Check(Check),
}

#[derive(Subcommand)]
enum Fetch {
    /// Receipts of the stored MON maker fills' transactions, stored as their
    /// request and fill logs; network, and needs `git` for the manifest row.
    FillTxs,
}

#[derive(Subcommand)]
enum Check {
    /// A holder who hedges on Perpl and later sells through the atomic exit,
    /// against waiting unhedged, before and after funding; offline.
    HedgedExit,
    /// Stored MON maker fills by the taker's side; reads perpetual decimals.
    Fills,
    /// Executable Perpl prices at the configured clip sizes; network.
    Depth,
    /// Funding received by a short over event blocks in (from, to]; offline.
    FundingSum {
        #[arg(long)]
        perp: u32,
        #[arg(long)]
        from: u64,
        #[arg(long)]
        to: u64,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config = Config::load(&cli.config)?;
    let chain = Chain::mainnet();
    match cli.command {
        Command::Ping => {
            rpc::connect(&config, &chain).await?;
            println!("chain {} at {}", chain.chain_id(), config.rpc_url);
        }
        Command::Fetch(Fetch::FillTxs) => fetch_fill_txs(&cli.config, &config, &chain).await?,
        Command::Check(Check::HedgedExit) => check_hedged_exit(&config)?,
        Command::Check(Check::Fills) => check_fills(&config, &chain).await?,
        Command::Check(Check::Depth) => {
            let provider = rpc::connect(&config, &chain).await?;
            let d = depth::measure(
                &provider,
                &chain,
                config.depth.perp_id,
                &config.depth.clip_usd,
            )
            .await?;
            println!(
                "block {} touch bid {:?} ask {:?}",
                d.block, d.touch_bid, d.touch_ask
            );
            write_json(
                &config
                    .paths
                    .derived
                    .join(format!("depth-{}.json", d.perp_id)),
                &d,
            )?;
        }
        Command::Check(Check::FundingSum { perp, from, to }) => {
            let intervals = load_intervals(&config)?;
            let iv = intervals
                .get(&perp)
                .with_context(|| format!("no funding events for perp {perp}"))?;
            let (sum_bps, n) = iv.short_receives_bps(from, to);
            println!(
                "{}",
                serde_json::json!({"perp_id": perp, "from_block": from, "to_block": to,
                    "intervals": n, "funding_to_short_bps": stats::round_to(sum_bps, 2)})
            );
        }
    }
    Ok(())
}

fn load_intervals(
    config: &Config,
) -> anyhow::Result<std::collections::BTreeMap<u32, funding::Intervals>> {
    let rows = rows::read_jsonl(&config.paths.raw.join("funding-events.jsonl"))?;
    Ok(funding::intervals(&funding::decode(&rows)?))
}

#[derive(Serialize)]
struct HedgedExitOutput {
    method: [&'static str; 3],
    markets: Vec<hedged_exit::MarketReport>,
}

fn check_hedged_exit(config: &Config) -> anyhow::Result<()> {
    let book_rows = rows::read_jsonl(&config.paths.raw.join("book-samples.jsonl"))?;
    let fee_rows = rows::read_jsonl(&config.paths.raw.join("fees.jsonl"))?;
    let fees = books::taker_fees_bps(&fee_rows)?;
    let perp_ids: Vec<u32> = config.markets.iter().map(|m| m.perp_id).collect();
    let series = books::samples(&book_rows, &fees, &perp_ids)?;
    let intervals = load_intervals(config)?;

    let markets = config
        .markets
        .iter()
        .map(|m| {
            Ok(hedged_exit::report(
                m.perp_id,
                &m.label,
                series
                    .get(&m.perp_id)
                    .with_context(|| format!("no samples for perp {}", m.perp_id))?,
                intervals
                    .get(&m.perp_id)
                    .with_context(|| format!("no funding events for perp {}", m.perp_id))?,
                config.hedged_exit.maker_hedge_fee_bps,
            ))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    for m in &markets {
        println!(
            "{} {}: exited {} of {}; median gross taker {:?} maker {:?}; net taker {:?} maker {:?}",
            m.market,
            m.outcome,
            m.exited,
            m.arrivals,
            m.gross_taker_hedge_bps.p50,
            m.gross_maker_hedge_bps.p50,
            m.net_taker_hedge_bps.p50,
            m.net_maker_hedge_bps.p50
        );
    }
    let output = HedgedExitOutput {
        method: [
            "Funding counts the events whose event block lies after the arrival sample and at or before the exit sample, on Perpl's documentation that a position receives each funding event it is held through; whether a position opened between a rate's emission and its event block receives it is not checked.",
            "Funding is the sum of actualRatePct100k x 0.1 bps of the funding price; the drift of notional between arrival and exit is ignored.",
            "Gross figures: the hedge price's premium to the Perpl oracle at arrival (Perpl bid for a taker hedge, Perpl ask for a maker hedge assumed filled), less the Perpl ask's premium to the oracle at the exit sample, less the fees; exit is the first later sample whose taker exit edge (Kuru bid against Perpl ask, less the taker fee) is at least 0.",
        ],
        markets,
    };
    write_json(&config.paths.derived.join("hedged-exit.json"), &output)
}

const FILL_TX_LOGS: &str = "fill-tx-logs.jsonl";

async fn fetch_fill_txs(config_path: &Path, config: &Config, chain: &Chain) -> anyhow::Result<()> {
    let provenance = manifest::provenance(config_path)?;
    let input = config.paths.raw.join("maker-fills.jsonl");
    let input_sha256 = manifest::sha256(&input)?;
    let fills: Vec<rows::MakerFillRow> = rows::read_jsonl(&input)?;
    let mut txs: Vec<_> = fills.iter().map(|f| f.tx).collect();
    txs.sort_unstable();
    txs.dedup();
    let blocks = || fills.iter().map(|f| f.block);
    let (from_block, to_block) = (blocks().min(), blocks().max());

    let provider = rpc::connect(config, chain).await?;
    let rows = fills::fetch(
        &provider,
        chain.exchange(),
        txs.clone(),
        config.rpc.in_flight,
    )
    .await?;
    let errors = rows
        .iter()
        .filter(|r| matches!(r, fills::TxLogRow::Error { .. }))
        .count();

    let path = config.paths.raw.join(FILL_TX_LOGS);
    let text: String = rows
        .iter()
        .map(|r| serde_json::to_string(r).map(|s| s + "\n"))
        .collect::<Result<_, _>>()?;
    std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    manifest::append(
        &config.paths.manifest,
        &serde_json::json!({
            "dataset": "fill-tx-logs", "rpc": config.rpc_url, "chain_id": chain.chain_id(),
            "from_block": from_block, "to_block": to_block, "transactions": txs.len(),
            "rows": rows.len(), "error_rows": errors,
            "input": {"path": input, "sha256": input_sha256},
            "note": "Request, fill and batch-completed logs of the Perpl exchange from the receipts of the transactions in raw/maker-fills.jsonl, as the chain returned them; an error row is a transaction whose receipt could not be read",
            "provenance": provenance,
        }),
    )?;
    println!(
        "wrote {} rows, {errors} error rows, for {} transactions",
        rows.len(),
        txs.len()
    );
    Ok(())
}

async fn check_fills(config: &Config, chain: &Chain) -> anyhow::Result<()> {
    let perp_id = config.fills.perp_id;
    let tx_logs: Vec<fills::TxLogRow> = rows::read_jsonl(&config.paths.raw.join(FILL_TX_LOGS))?;
    let stored: Vec<rows::MakerFillRow> =
        rows::read_jsonl(&config.paths.raw.join("maker-fills.jsonl"))?;
    let block_times: Vec<(u64, u64)> =
        rows::read_jsonl::<rows::BlockTimeRow>(&config.paths.raw.join("block-times.jsonl"))?
            .into_iter()
            .map(|r| {
                let t = r
                    .timestamp
                    .with_context(|| format!("block {}: no timestamp", r.block))?;
                Ok((r.block, t.to()))
            })
            .collect::<anyhow::Result<_>>()?;

    let provider = rpc::connect(config, chain).await?;
    let normalization =
        NormalizationConfig::fetch(&chain.clone().with_perpetuals(vec![perp_id]), &provider)
            .await
            .context("reading perpetual decimals")?;
    let matches = fills::match_fills(&tx_logs, &normalization, perp_id)?;
    let report = fills::report(
        perp_id,
        &stored,
        &block_times,
        &matches,
        config.fills.max_unmatched_share,
    )?;
    let c = &report.counts;
    println!(
        "fills {}: ask-side {}, bid-side {}, unmatched (no receipt {}, not in a trade {}), share {:.4}; V1 events {}",
        report.fills,
        c.ask_side,
        c.bid_side,
        c.unmatched_no_receipt,
        c.unmatched_not_in_trade,
        report.unmatched_share,
        report.v1_events_in_receipts
    );
    write_json(&config.paths.derived.join("fills-taker-side.json"), &report)
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let text = serde_json::to_string_pretty(value)?;
    std::fs::write(path, text + "\n").with_context(|| format!("writing {}", path.display()))?;
    println!("wrote {}", path.display());
    Ok(())
}
