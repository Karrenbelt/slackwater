//! The keeper: reads the instance, decides, and with `run --send` signs as
//! the keeper, one transaction at a time. Without `--send` no key is loaded:
//! each action is simulated and the record line it would write is printed.

use std::{io::Write, path::PathBuf, time::Duration};

use alloy::{
    consensus::Transaction as _,
    network::Ethereum,
    primitives::U256,
    providers::{DynProvider, PendingTransactionBuilder, Provider, ProviderBuilder},
    signers::local::PrivateKeySigner,
};
use anyhow::{Context, bail, ensure};
use clap::{Parser, Subcommand};
use perpl_sdk::{Chain, abi::errors::Exchange::ExchangeErrors};
use secrecy::{ExposeSecret, SecretString};
use serde_json::{Value, json};
use slackwater::{
    bindings::BasisTrader::{self, BasisTraderErrors, HedgeParams},
    chain::{self, Statics},
    keeper::{self, Action, CancelReason, Params, SignalKind, State},
    keeper_config::KeeperConfig,
    record::{self, ClipRecord, Ctx, Mined},
};
use tracing::{error, info, warn};

const WEI_PER_MON: u128 = 1_000_000_000_000_000_000;

#[derive(Parser)]
#[command(about = "The slackwater keeper: posts, requotes and records a clip; dry run by default")]
struct Cli {
    #[arg(long, default_value = "config/keeper.toml")]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Read once at the latest block, decide, and print both as JSON.
    Once,
    /// Read, decide and act every `poll_sec`; dry run unless `--send`.
    Run {
        /// Sign and send as the keeper. Without it, nothing is signed.
        #[arg(long)]
        send: bool,
        /// The keeper's Foundry keystore; opened only with `--send`.
        #[arg(long)]
        keystore: Option<PathBuf>,
        /// Stop after this many cycles; without it, run until stopped.
        #[arg(long)]
        cycles: Option<u64>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    let cfg = KeeperConfig::load(&cli.config)?;
    match cli.command {
        Command::Once => {
            let provider = connect(&cfg, None).await?;
            let block = chain::latest_block(&provider).await?;
            let statics = start_checks(&provider, &cfg, block).await?;
            let params = params(&cfg, &statics);
            let (state, _) = read(&provider, &cfg, &statics, block).await?;
            let decision = keeper::decide(&state, &params);
            let state = serde_json::to_value(&state).context("serialising the state")?;
            let decision = serde_json::to_value(&decision).context("serialising the decision")?;
            println!(
                "{}",
                Value::from_iter([("state", state), ("decision", decision)])
            );
            Ok(())
        }
        Command::Run {
            send,
            keystore,
            cycles,
        } => {
            let signer = if send {
                let path = keystore.context("--send needs --keystore")?;
                Some(load_signer(&path, &cfg)?)
            } else {
                None
            };
            let provider = connect(&cfg, signer).await?;
            let block = chain::latest_block(&provider).await?;
            let statics = start_checks(&provider, &cfg, block).await?;
            info!(
                send,
                block,
                account = statics.account_id,
                discount_bps = statics.discount_bps,
                "keeper started"
            );
            run(&provider, &cfg, &statics, send, cycles).await
        }
    }
}

/// The password is read without echo into a `SecretString` and exposed only
/// to the keystore's decryption.
fn load_signer(path: &std::path::Path, cfg: &KeeperConfig) -> anyhow::Result<PrivateKeySigner> {
    let password: SecretString = dialoguer::Password::new()
        .with_prompt(format!("password for {}", path.display()))
        .interact()
        .context("reading the keystore password (needs a terminal)")?
        .into();
    let signer = PrivateKeySigner::decrypt_keystore(path, password.expose_secret())
        .with_context(|| format!("opening the keystore {}", path.display()))?;
    ensure!(
        signer.address() == cfg.instance.keeper,
        "the keystore is {}, the configured keeper is {}",
        signer.address(),
        cfg.instance.keeper
    );
    Ok(signer)
}

async fn connect(
    cfg: &KeeperConfig,
    signer: Option<PrivateKeySigner>,
) -> anyhow::Result<DynProvider> {
    let builder = ProviderBuilder::new();
    let provider = match signer {
        Some(s) => builder
            .wallet(s)
            .connect(&cfg.rpc_url)
            .await
            .map(Provider::erased),
        None => builder.connect(&cfg.rpc_url).await.map(Provider::erased),
    };
    provider.with_context(|| format!("connecting to {}", cfg.rpc_url))
}

async fn start_checks(
    provider: &DynProvider,
    cfg: &KeeperConfig,
    block: u64,
) -> anyhow::Result<Statics> {
    let chain = Chain::mainnet();
    let chain_id = provider
        .get_chain_id()
        .await
        .context("reading the chain id")?;
    ensure!(
        chain_id == chain.chain_id(),
        "{} serves chain {chain_id}, expected {}",
        cfg.rpc_url,
        chain.chain_id()
    );
    let statics = chain::read_statics(provider, &cfg.instance, block).await?;
    ensure!(
        statics.perpl == chain.exchange(),
        "the instance's PERPL() is {}, the SDK's exchange is {}",
        statics.perpl,
        chain.exchange()
    );
    ensure!(
        cfg.clip.oracle_age_to_sign_sec <= statics.max_oracle_age_sec,
        "oracle_age_to_sign_sec {} exceeds the instance's hedgeMaxOracleAgeSec {}",
        cfg.clip.oracle_age_to_sign_sec,
        statics.max_oracle_age_sec
    );
    if !statics.is_keeper {
        bail!(
            "{} is not a keeper of {}",
            cfg.instance.keeper,
            cfg.instance.address
        );
    }
    Ok(statics)
}

fn params(cfg: &KeeperConfig, statics: &Statics) -> Params {
    let s = &cfg.stop;
    Params {
        clip_lots: cfg.clip.lots,
        max_posts: cfg.clip.max_posts,
        requote_sec: cfg.clip.requote_min * 60,
        oracle_age_to_sign_sec: cfg.clip.oracle_age_to_sign_sec,
        discount_bps: statics.discount_bps,
        top_up_mark_bps: s.top_up_mark_bps,
        mark_rise_signal_bps: s.mark_rise_signal_bps,
        funding_signal_bps: s.funding_signal_bps,
        exit_guard_bps: s.exit_guard_bps,
        signal_repeat_sec: s.signal_repeat_min * 60,
        gas_signal_wei: u128::from(s.gas_signal_mon) * WEI_PER_MON,
        gas_stop_wei: u128::from(s.gas_stop_mon) * WEI_PER_MON,
    }
}

async fn read(
    provider: &DynProvider,
    cfg: &KeeperConfig,
    statics: &Statics,
    block: u64,
) -> anyhow::Result<(State, ClipRecord)> {
    let rec = record::read_clip(&cfg.record.path, cfg.record.round, cfg.record.clip)?;
    let state = chain::read_state(provider, &cfg.instance, statics, &rec, block).await?;
    Ok((state, rec))
}

enum Outcome {
    Continue,
    Stop(String),
}

async fn run(
    provider: &DynProvider,
    cfg: &KeeperConfig,
    statics: &Statics,
    send: bool,
    cycles: Option<u64>,
) -> anyhow::Result<()> {
    let params = params(cfg, statics);
    let mut failures = 0;
    let mut cycle = 0;
    while cycles.is_none_or(|c| cycle < c) {
        cycle += 1;
        match one_cycle(provider, cfg, statics, &params, send).await {
            Ok(Outcome::Continue) => failures = 0,
            Ok(Outcome::Stop(why)) => {
                error!(%why, "keeper stopped; positions are as they are on chain");
                bail!("stopped: {why}");
            }
            Err(e) => {
                failures += 1;
                warn!(error = %format!("{e:#}"), failures, "cycle skipped");
                if failures >= cfg.run.max_failed_cycles {
                    bail!(
                        "{failures} cycles in a row failed; stopping with positions as they are on chain"
                    );
                }
            }
        }
        if cycles.is_none_or(|c| cycle < c) {
            tokio::time::sleep(Duration::from_secs(cfg.run.poll_sec)).await;
        }
    }
    Ok(())
}

async fn one_cycle(
    provider: &DynProvider,
    cfg: &KeeperConfig,
    statics: &Statics,
    params: &Params,
    send: bool,
) -> anyhow::Result<Outcome> {
    let block = chain::latest_block(provider).await?;
    let (state, rec) = read(provider, cfg, statics, block).await?;
    let decision = keeper::decide(&state, params);
    info!(
        block,
        action = %serde_json::to_string(&decision.action).context("serialising the action")?,
        position_lots = state.position.lots,
        resting_hedge = state.resting_hedge,
        "decided"
    );
    if !send {
        let s = serde_json::to_value(&state).context("serialising the state")?;
        let d = serde_json::to_value(&decision).context("serialising the decision")?;
        println!("{}", Value::from_iter([("state", s), ("decision", d)]));
    }
    let ctx = Ctx {
        round: cfg.record.round,
        clip: cfg.record.clip,
        time_sec: state.time_sec,
    };
    let out = Out { cfg, send };
    for kind in &decision.signals {
        signal(&out, ctx, &state, cfg, *kind)?;
    }
    let act = Act {
        provider,
        cfg,
        statics,
        state: &state,
        rec: &rec,
        ctx,
        out: &out,
    };
    match decision.action {
        Action::Wait { reason } => {
            info!(?reason, "waiting");
            Ok(Outcome::Continue)
        }
        Action::Stop { reason } => Ok(Outcome::Stop(reason.advice())),
        Action::Post { lots, price_pns } => act.post(lots, price_pns).await,
        Action::Cancel { order_id, reason } => act.cancel(order_id, reason).await,
        Action::TopUp { amount_cns } => act.top_up(amount_cns).await,
    }
}

/// Where lines go: the record when sending, standard output marked
/// `dryRun` otherwise.
struct Out<'a> {
    cfg: &'a KeeperConfig,
    send: bool,
}

impl Out<'_> {
    fn line(&self, mut line: Value) -> anyhow::Result<()> {
        if self.send {
            record::append(&self.cfg.record.path, &line)?;
        } else if let Value::Object(m) = &mut line {
            m.insert("dryRun".into(), json!(true));
            println!("{line}");
        }
        Ok(())
    }
}

/// A signal: a greppable WARN line, a bell, and a `signal-<kind>` line. Only
/// an attended run hears it.
fn signal(
    out: &Out,
    ctx: Ctx,
    s: &State,
    cfg: &KeeperConfig,
    kind: SignalKind,
) -> anyhow::Result<()> {
    let advice = match kind {
        SignalKind::MarkRise => format!(
            "the mark is at least {} bps of the entry price: consider EXIT_GUARD_BPS={} scripts/round/round.sh forceexit",
            cfg.stop.mark_rise_signal_bps, cfg.stop.widened_exit_guard_bps
        ),
        SignalKind::Funding => format!(
            "funding paid since the fill is at least {} bps of the entry notional: consider scripts/round/round.sh forceexit",
            cfg.stop.funding_signal_bps
        ),
        SignalKind::ExitEdge => {
            "the touch exit edge is at least 0 and the guard holds: scripts/round/round.sh forceexit if you want to exit".to_string()
        }
        SignalKind::Margin => "the top-up threshold is reached and the Perpl account has no free balance".to_string(),
        SignalKind::LowGas => format!("the keeper holds under {} MON", cfg.stop.gas_signal_mon),
    };
    warn!(signal = kind.as_str(), block = s.block, %advice, "SIGNAL");
    eprint!("\x07");
    std::io::stderr().flush().context("ringing the bell")?;
    out.line(record::read_line(
        ctx,
        &format!("signal-{}", kind.as_str()),
        record::fields([
            ("block", json!(s.block)),
            ("advice", json!(advice)),
            ("markPNS", json!(s.position.mark_pns)),
            ("entryPNS", json!(s.position.entry_pns)),
            (
                "premiumPnlCNS",
                json!(s.position.premium_pnl_cns.to_string()),
            ),
        ]),
    ))
}

struct Act<'a> {
    provider: &'a DynProvider,
    cfg: &'a KeeperConfig,
    statics: &'a Statics,
    state: &'a State,
    rec: &'a ClipRecord,
    ctx: Ctx,
    out: &'a Out<'a>,
}

/// A failed simulation, decoded where the contract or Perpl knows the error.
fn revert_name(e: &alloy::contract::Error) -> String {
    if let Some(err) = e.as_decoded_interface_error::<BasisTraderErrors>() {
        return format!("{err:?}");
    }
    if let Some(err) = e.as_decoded_interface_error::<ExchangeErrors>() {
        return format!("{err:?}");
    }
    format!("{e}")
}

fn margin_lag(e: &alloy::contract::Error) -> bool {
    matches!(
        e.as_decoded_interface_error::<ExchangeErrors>(),
        Some(ExchangeErrors::AmountExceedsAvailableBalance(_))
    )
}

impl Act<'_> {
    fn perp(&self) -> U256 {
        U256::from(self.cfg.instance.perp_id)
    }

    fn contract(&self) -> BasisTrader::BasisTraderInstance<&DynProvider> {
        BasisTrader::new(self.cfg.instance.address, self.provider)
    }

    /// Waits for the receipt up to the timeout; a send or receipt failure
    /// stops the keeper, because whether anything was sent is then unknown.
    async fn mined(
        &self,
        pending: Result<PendingTransactionBuilder<Ethereum>, alloy::contract::Error>,
    ) -> Result<Mined, Outcome> {
        let advice = "check the keeper's nonce before restarting";
        let pending = pending.map_err(|e| {
            Outcome::Stop(format!("the send failed before a receipt ({e}): {advice}"))
        })?;
        let hash = *pending.tx_hash();
        let receipt = pending
            .with_timeout(Some(Duration::from_secs(self.cfg.run.receipt_timeout_sec)))
            .get_receipt()
            .await
            .map_err(|e| Outcome::Stop(format!("no receipt for {hash} ({e}): {advice}")))?;
        // A mined transaction is always recorded; an unread limit is null.
        let gas_limit = match self.provider.get_transaction_by_hash(hash).await {
            Ok(Some(t)) => Some(t.inner.gas_limit()),
            Ok(None) => None,
            Err(e) => {
                warn!(%hash, error = %e, "the gas limit could not be read; recorded as null");
                None
            }
        };
        Mined::from_receipt(&receipt, gas_limit).ok_or_else(|| {
            Outcome::Stop(format!(
                "{hash} has no block in its receipt: record it with round.sh rec"
            ))
        })
    }

    /// Retried, because the RPC can lag the receipt's block, and a failure
    /// here leaves a mined transaction without its line until the next
    /// cycle stops on `RecordBehindChain`.
    async fn after(&self, block: u64) -> anyhow::Result<State> {
        let mut tries = 0;
        loop {
            tries += 1;
            match read(self.provider, self.cfg, self.statics, block).await {
                Ok((state, _)) => return Ok(state),
                Err(e) if tries < 3 => {
                    warn!(block, error = %format!("{e:#}"), tries, "read after the receipt failed; retrying");
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
                Err(e) => return Err(e.context(format!("reading block {block} after the receipt"))),
            }
        }
    }

    async fn post(&self, lots: u64, price_pns: u64) -> anyhow::Result<Outcome> {
        let s = self.state;
        let p = HedgeParams {
            perpId: self.perp(),
            lotLNS: U256::from(lots),
            pricePNS: U256::from(price_pns),
            marginCNS: U256::ZERO,
            requestId: U256::from(s.block),
        };
        let bt = self.contract();
        let call = bt.hedge(p).from(self.cfg.instance.keeper);
        let order_id = match call.call().await {
            Ok(id) => id,
            Err(e) if margin_lag(&e) => {
                info!("the cancel's margin release has not settled; posting next cycle");
                return Ok(Outcome::Continue);
            }
            Err(e) => bail!("simulating hedge: {}", revert_name(&e)),
        };
        let step = self.rec.next_step("hedge-");
        let acct_before = record::read_line(
            self.ctx,
            "acct-before-post",
            post_index(record::acct_fields(s, self.statics.account_id), &step),
        );
        if !self.out.send {
            info!(%step, lots, price_pns, simulated_order_id = %order_id, "would post");
            self.out.line(acct_before)?;
            let order_id =
                u64::try_from(order_id).context("the simulated order ID does not fit a u64")?;
            let mut line = record::read_line(
                self.ctx,
                &step,
                record::hedge_fields(s, self.statics.discount_bps, lots, price_pns, order_id),
            );
            if let Value::Object(m) = &mut line {
                m.insert("simulated".into(), json!(true));
            }
            return self.out.line(line).map(|()| Outcome::Continue);
        }
        let mined = match self.mined(call.send().await).await {
            Ok(m) => m,
            Err(o) => return Ok(o),
        };
        let after = self.after(mined.block).await?;
        self.out.line(acct_before)?;
        let line = record::tx_line(
            self.ctx,
            &step,
            &mined,
            reverted(
                record::hedge_fields(
                    s,
                    self.statics.discount_bps,
                    lots,
                    price_pns,
                    after.resting_hedge,
                ),
                &mined,
            ),
        );
        self.out.line(line)?;
        self.out.line(record::read_line(
            self.ctx,
            "acct-after-post",
            post_index(record::acct_fields(&after, self.statics.account_id), &step),
        ))?;
        info!(%step, tx = %mined.tx, status = mined.status, order_id = after.resting_hedge, "posted");
        ensure!(mined.status == 1, "{} reverted; it is recorded", mined.tx);
        Ok(Outcome::Continue)
    }

    async fn cancel(&self, order_id: u64, reason: CancelReason) -> anyhow::Result<Outcome> {
        let s = self.state;
        let ours = s.order.as_ref().is_some_and(|o| o.ours);
        let bt = self.contract();
        let call = bt
            .cancelHedge(self.perp(), U256::from(s.block))
            .from(self.cfg.instance.keeper);
        let unfilled = call
            .call()
            .await
            .map_err(|e| anyhow::anyhow!("simulating cancelHedge: {}", revert_name(&e)))?;
        let unfilled = u64::try_from(unfilled).context("the unfilled lots do not fit a u64")?;
        let step = match reason {
            CancelReason::Filled => "cancel-final".to_string(),
            CancelReason::Requote | CancelReason::OrderGone => self.rec.next_step("cancel-"),
        };
        if !self.out.send {
            info!(%step, order_id, ?reason, unfilled, "would cancel");
            let mut fields = record::cancel_fields(order_id, ours, unfilled, s);
            fields.insert("simulated".into(), json!(true));
            return self
                .out
                .line(record::read_line(self.ctx, &step, fields))
                .map(|()| Outcome::Continue);
        }
        let mined = match self.mined(call.send().await).await {
            Ok(m) => m,
            Err(o) => return Ok(o),
        };
        let after = self.after(mined.block).await?;
        self.out.line(record::tx_line(
            self.ctx,
            &step,
            &mined,
            reverted(
                record::cancel_fields(order_id, ours, unfilled, &after),
                &mined,
            ),
        ))?;
        info!(%step, tx = %mined.tx, status = mined.status, ?reason, "cancelled");
        ensure!(mined.status == 1, "{} reverted; it is recorded", mined.tx);
        match reason {
            CancelReason::Filled => self.out.line(record::read_line(
                self.ctx,
                "acct-after-fill",
                record::acct_fields(&after, self.statics.account_id),
            ))?,
            CancelReason::OrderGone => self.out.line(record::read_line(
                self.ctx,
                "order-gone",
                record::fields([
                    ("orderId", json!(order_id)),
                    ("block", json!(s.block)),
                    ("positionLots", json!(after.position.lots)),
                ]),
            ))?,
            CancelReason::Requote => {}
        }
        Ok(Outcome::Continue)
    }

    async fn top_up(&self, amount_cns: u128) -> anyhow::Result<Outcome> {
        let s = self.state;
        let bt = self.contract();
        let call = bt
            .topUpPosition(self.perp(), U256::from(amount_cns))
            .from(self.cfg.instance.keeper);
        call.call()
            .await
            .map_err(|e| anyhow::anyhow!("simulating topUpPosition: {}", revert_name(&e)))?;
        let step = self.rec.next_step("top-up-");
        let fields = |after: &State| {
            record::fields([
                ("amountCNS", record::num(amount_cns)),
                ("markPNS", json!(s.position.mark_pns)),
                ("entryPNS", json!(s.position.entry_pns)),
                ("positionAfter", record::pos_fields(after)),
            ])
        };
        if !self.out.send {
            info!(%step, amount_cns, "would top up");
            let mut f = fields(s);
            f.insert("simulated".into(), json!(true));
            return self
                .out
                .line(record::read_line(self.ctx, &step, f))
                .map(|()| Outcome::Continue);
        }
        let mined = match self.mined(call.send().await).await {
            Ok(m) => m,
            Err(o) => return Ok(o),
        };
        let after = self.after(mined.block).await?;
        self.out.line(record::tx_line(
            self.ctx,
            &step,
            &mined,
            reverted(fields(&after), &mined),
        ))?;
        info!(%step, tx = %mined.tx, status = mined.status, "topped up");
        ensure!(mined.status == 1, "{} reverted; it is recorded", mined.tx);
        Ok(Outcome::Continue)
    }
}

/// `post` names the hedge line an `acct-*-post` read belongs to, as in
/// `round.sh`.
fn post_index(
    mut fields: serde_json::Map<String, Value>,
    step: &str,
) -> serde_json::Map<String, Value> {
    if let Some(k) = step
        .strip_prefix("hedge-")
        .and_then(|k| k.parse::<u64>().ok())
    {
        fields.insert("post".into(), json!(k));
    }
    fields
}

/// A mined transaction that reverted paid gas, so it is recorded, marked.
fn reverted(
    mut fields: serde_json::Map<String, Value>,
    mined: &Mined,
) -> serde_json::Map<String, Value> {
    if mined.status != 1 {
        fields.insert("reverted".into(), json!(true));
    }
    fields
}
