//! Reads of the instance, Perpl and Kuru, all at one block, into the keeper's
//! `State`.

use alloy::{
    eips::{BlockId, BlockNumberOrTag},
    primitives::{Address, U256},
    providers::Provider,
};
use perpl_sdk::abi::dex::Exchange;

use crate::{
    bindings::{BasisTrader, KuruOrderBook},
    keeper::{Account, ClipMemory, KuruTouch, LastPost, PerpView, Position, RestingOrder, State},
    keeper_config::Instance,
    record::ClipRecord,
};

const WEI_PER_MON: u128 = 1_000_000_000_000_000_000;
/// `BasisTrader.POSITION_SHORT`.
const POSITION_SHORT: u8 = 1;

#[derive(Debug, thiserror::Error)]
pub enum ChainError {
    #[error("reading {what} at block {block}: {source}")]
    Call {
        what: &'static str,
        block: u64,
        source: Box<alloy::contract::Error>,
    },
    #[error("reading {what} at block {block}: {source}")]
    Rpc {
        what: &'static str,
        block: u64,
        source: alloy::transports::TransportError,
    },
    #[error("block {block} is not served")]
    NoBlock { block: u64 },
    #[error("{what} at block {block} does not fit: {value}")]
    NotFit {
        what: &'static str,
        block: u64,
        value: String,
    },
}

/// Read once at start.
#[derive(Debug, Clone)]
pub struct Statics {
    pub account_id: u64,
    pub discount_bps: u64,
    pub max_oracle_age_sec: u64,
    pub perpl: Address,
    pub is_keeper: bool,
}

fn fit<T: TryFrom<U256>>(what: &'static str, block: u64, v: U256) -> Result<T, ChainError> {
    T::try_from(v).map_err(|_| ChainError::NotFit {
        what,
        block,
        value: v.to_string(),
    })
}

fn call_err(what: &'static str, block: u64) -> impl FnOnce(alloy::contract::Error) -> ChainError {
    move |source| ChainError::Call {
        what,
        block,
        source: Box::new(source),
    }
}

fn rpc_err(
    what: &'static str,
    block: u64,
) -> impl FnOnce(alloy::transports::TransportError) -> ChainError {
    move |source| ChainError::Rpc {
        what,
        block,
        source,
    }
}

pub async fn latest_block<P: Provider>(provider: &P) -> Result<u64, ChainError> {
    provider
        .get_block_number()
        .await
        .map_err(rpc_err("the block number", 0))
}

pub async fn block_time<P: Provider>(provider: &P, block: u64) -> Result<u64, ChainError> {
    provider
        .get_block_by_number(BlockNumberOrTag::Number(block))
        .await
        .map_err(rpc_err("the block", block))?
        .map(|b| b.header.timestamp)
        .ok_or(ChainError::NoBlock { block })
}

pub async fn read_statics<P: Provider>(
    provider: &P,
    cfg: &Instance,
    block: u64,
) -> Result<Statics, ChainError> {
    let at = BlockId::number(block);
    let bt = BasisTrader::new(cfg.address, provider);
    let perp = U256::from(cfg.perp_id);
    Ok(Statics {
        account_id: fit(
            "perplAccountId",
            block,
            bt.perplAccountId()
                .block(at)
                .call()
                .await
                .map_err(call_err("perplAccountId", block))?,
        )?,
        discount_bps: u64::from(
            bt.hedgeMaxDiscountBps(perp)
                .block(at)
                .call()
                .await
                .map_err(call_err("hedgeMaxDiscountBps", block))?,
        ),
        max_oracle_age_sec: u64::from(
            bt.hedgeMaxOracleAgeSec(perp)
                .block(at)
                .call()
                .await
                .map_err(call_err("hedgeMaxOracleAgeSec", block))?,
        ),
        perpl: bt
            .PERPL()
            .block(at)
            .call()
            .await
            .map_err(call_err("PERPL", block))?,
        is_keeper: bt
            .keepers(cfg.keeper)
            .block(at)
            .call()
            .await
            .map_err(call_err("keepers", block))?,
    })
}

/// Everything `decide` needs, read at `block`; the clip's memory comes from
/// the record, with the last post's time from its block.
pub async fn read_state<P: Provider>(
    provider: &P,
    cfg: &Instance,
    statics: &Statics,
    record: &ClipRecord,
    block: u64,
) -> Result<State, ChainError> {
    let at = BlockId::number(block);
    let perp = U256::from(cfg.perp_id);
    let bt = BasisTrader::new(cfg.address, provider);
    let ex = Exchange::new(statics.perpl, provider);
    let book = KuruOrderBook::new(cfg.kuru_book, provider);

    let time_sec = block_time(provider, block).await?;
    let info = ex
        .getPerpetualInfoV2(perp)
        .block(at)
        .call()
        .await
        .map_err(call_err("getPerpetualInfoV2", block))?;
    let fee = ex
        .getTakerFee(perp)
        .block(at)
        .call()
        .await
        .map_err(call_err("getTakerFee", block))?;
    let touch = book
        .bestBidAsk()
        .block(at)
        .call()
        .await
        .map_err(call_err("bestBidAsk", block))?;
    let resting_hedge: u64 = fit(
        "restingHedge",
        block,
        bt.restingHedge(perp)
            .block(at)
            .call()
            .await
            .map_err(call_err("restingHedge", block))?,
    )?;
    let pos = bt
        .position(perp)
        .block(at)
        .call()
        .await
        .map_err(call_err("position", block))?;
    let acct = bt
        .perplAccount()
        .block(at)
        .call()
        .await
        .map_err(call_err("perplAccount", block))?;
    let order = if resting_hedge == 0 {
        None
    } else {
        let o = ex
            .getOrderV2(perp, U256::from(resting_hedge))
            .block(at)
            .call()
            .await
            .map_err(call_err("getOrderV2", block))?;
        Some(RestingOrder {
            order_id: resting_hedge,
            ours: u64::from(o.accountId) == statics.account_id,
            lots: o.lotLNS.to::<u64>(),
            price_pns: o.priceONS.to::<u64>(),
        })
    };
    let instance_wei: u128 = fit(
        "the instance's balance",
        block,
        provider
            .get_balance(cfg.address)
            .block_id(at)
            .await
            .map_err(rpc_err("the instance's balance", block))?,
    )?;
    let keeper_wei: u128 = fit(
        "the keeper's balance",
        block,
        provider
            .get_balance(cfg.keeper)
            .block_id(at)
            .await
            .map_err(rpc_err("the keeper's balance", block))?,
    )?;
    let last_post = match &record.last_post {
        None => None,
        Some(lp) => Some(LastPost {
            order_id: lp.order_id,
            lots: lp.lots,
            position_lots_before: lp.position_lots_before,
            block: lp.block,
            time_sec: block_time(provider, lp.block).await?,
            cancelled: lp.cancelled,
        }),
    };

    let oracle_ts: u64 = fit("oracleTimestampSec", block, info.oracleTimestampSec)?;
    Ok(State {
        block,
        time_sec,
        perp: PerpView {
            oracle_pns: fit("oraclePNS", block, info.oraclePNS)?,
            oracle_age_sec: time_sec.saturating_sub(oracle_ts),
            ign_oracle: info.ignOracle,
            bid_pns: fit("maxBidPriceONS", block, info.maxBidPriceONS)?,
            ask_pns: fit("minAskPriceONS", block, info.minAskPriceONS)?,
            taker_fee_ppm: fit("getTakerFee", block, fee)?,
            perp_mark_pns: fit("markPNS", block, info.markPNS)?,
            funding_rate_pct100k: info.fundingRatePct100k,
        },
        kuru: KuruTouch {
            bid_e18: fit("Kuru bid", block, touch._0)?,
            ask_e18: fit("Kuru ask", block, touch._1)?,
        },
        account: Account {
            balance_cns: fit("balanceCNS", block, acct.balanceCNS)?,
            locked_cns: fit("lockedBalanceCNS", block, acct.lockedBalanceCNS)?,
        },
        position: Position {
            short: pos.info.positionType == POSITION_SHORT,
            lots: fit("lotLNS", block, pos.info.lotLNS)?,
            deposit_cns: fit("depositCNS", block, pos.info.depositCNS)?,
            entry_pns: fit("pricePNS", block, pos.info.pricePNS)?,
            premium_pnl_cns: i128::try_from(pos.info.premiumPnlCNS).map_err(|_| {
                ChainError::NotFit {
                    what: "premiumPnlCNS",
                    block,
                    value: pos.info.premiumPnlCNS.to_string(),
                }
            })?,
            mark_pns: fit("markPricePNS", block, pos.markPricePNS)?,
            mark_valid: pos.markValid,
        },
        resting_hedge,
        order,
        instance_mon_lots: u64::try_from(instance_wei / WEI_PER_MON).map_err(|_| {
            ChainError::NotFit {
                what: "the instance's MON",
                block,
                value: instance_wei.to_string(),
            }
        })?,
        keeper_wei,
        clip: ClipMemory {
            base_lots: record.base_lots,
            mined_posts: record.mined_posts,
            last_post,
            signals_fired_sec: record.signals_fired_sec.clone(),
        },
    })
}
