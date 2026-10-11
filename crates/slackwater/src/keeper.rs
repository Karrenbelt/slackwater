//! The keeper's decision: a pure function from the chain's state, the clip's
//! record and the parameters to one action and any signals.
//!
//! Units follow the venues: Perpl prices in PNS and amounts in CNS (6
//! decimals on MON), lots of one MON, the Kuru touch in USDC per MON scaled
//! by 1e18, ratios in bps or ppm.

use std::collections::BTreeMap;

use serde::{Serialize, Serializer};

/// Amounts above 2^53 are strings, as in the record, and serde_json cannot
/// write a `u128` above `u64::MAX` as a number.
fn as_string<T: ToString, S: Serializer>(v: &T, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&v.to_string())
}

/// The keeper's view of the chain at one block, and of the clip's record.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct State {
    pub block: u64,
    pub time_sec: u64,
    pub perp: PerpView,
    pub kuru: KuruTouch,
    pub account: Account,
    pub position: Position,
    /// `BasisTrader.restingHedge(perpId)`: 0 when no hedge is recorded.
    pub resting_hedge: u64,
    /// The order `resting_hedge` names, if one is recorded.
    pub order: Option<RestingOrder>,
    pub instance_mon_lots: u64,
    #[serde(serialize_with = "as_string")]
    pub keeper_wei: u128,
    pub clip: ClipMemory,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PerpView {
    pub oracle_pns: u64,
    pub oracle_age_sec: u64,
    pub ign_oracle: bool,
    pub bid_pns: u64,
    pub ask_pns: u64,
    pub taker_fee_ppm: u64,
    /// The perpetual's own mark and funding rate, for the record only.
    pub perp_mark_pns: u64,
    pub funding_rate_pct100k: i16,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KuruTouch {
    #[serde(serialize_with = "as_string")]
    pub bid_e18: u128,
    #[serde(serialize_with = "as_string")]
    pub ask_e18: u128,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    /// Includes the order lock.
    #[serde(serialize_with = "as_string")]
    pub balance_cns: u128,
    #[serde(serialize_with = "as_string")]
    pub locked_cns: u128,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Position {
    pub short: bool,
    pub lots: u64,
    #[serde(serialize_with = "as_string")]
    pub deposit_cns: u128,
    pub entry_pns: u64,
    /// Cumulative funding, negative when the position paid.
    #[serde(serialize_with = "as_string")]
    pub premium_pnl_cns: i128,
    /// `position()`'s `markPricePNS`, with `markValid`.
    pub mark_pns: u64,
    pub mark_valid: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestingOrder {
    pub order_id: u64,
    /// The order's account is ours: Perpl reuses an ID once an order leaves
    /// the book.
    pub ours: bool,
    pub lots: u64,
    pub price_pns: u64,
}

/// What the record says about this clip.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipMemory {
    /// The position's lots before the clip's first post.
    pub base_lots: Option<u64>,
    /// `hedge-` lines with status 1.
    pub mined_posts: usize,
    pub last_post: Option<LastPost>,
    pub signals_fired_sec: BTreeMap<SignalKind, u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LastPost {
    pub order_id: u64,
    pub lots: u64,
    pub position_lots_before: u64,
    pub block: u64,
    pub time_sec: u64,
    /// A cancel line follows it in the record.
    pub cancelled: bool,
}

#[derive(Debug, Clone)]
pub struct Params {
    pub clip_lots: u64,
    pub max_posts: usize,
    pub requote_sec: u64,
    pub oracle_age_to_sign_sec: u64,
    /// The instance's `hedgeMaxDiscountBps`, read from the chain.
    pub discount_bps: u64,
    pub top_up_mark_bps: u64,
    pub mark_rise_signal_bps: u64,
    pub funding_signal_bps: u64,
    pub exit_guard_bps: u64,
    pub signal_repeat_sec: u64,
    pub gas_signal_wei: u128,
    pub gas_stop_wei: u128,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Decision {
    pub action: Action,
    pub signals: Vec<SignalKind>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum Action {
    Post {
        lots: u64,
        price_pns: u64,
    },
    Cancel {
        order_id: u64,
        reason: CancelReason,
    },
    TopUp {
        #[serde(serialize_with = "as_string")]
        amount_cns: u128,
    },
    Wait {
        reason: WaitReason,
    },
    Stop {
        reason: StopReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CancelReason {
    Requote,
    OrderGone,
    Filled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum WaitReason {
    OracleStale,
    OracleIgnored,
    NoSpread,
    BelowFloor,
    Resting,
    Holding,
    MarkInvalid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "stop", rename_all = "kebab-case")]
pub enum StopReason {
    RecordBehindChain { order_id: u64, block: u64 },
    NotShort,
    ShortAboveSpot,
    NoSpotToHedge,
    PostsUsed,
    LowGas,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SignalKind {
    MarkRise,
    Funding,
    ExitEdge,
    Margin,
    LowGas,
}

impl SignalKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SignalKind::MarkRise => "mark-rise",
            SignalKind::Funding => "funding",
            SignalKind::ExitEdge => "exit-edge",
            SignalKind::Margin => "margin",
            SignalKind::LowGas => "low-gas",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        [
            SignalKind::MarkRise,
            SignalKind::Funding,
            SignalKind::ExitEdge,
            SignalKind::Margin,
            SignalKind::LowGas,
        ]
        .into_iter()
        .find(|k| k.as_str() == name)
    }
}

impl StopReason {
    /// What the user does next, for the stop message.
    pub fn advice(&self) -> String {
        match self {
            StopReason::RecordBehindChain { order_id, block } => format!(
                "the record does not match the chain at block {block} (restingHedge {order_id}): write the missing line with round.sh rec before restarting"
            ),
            StopReason::NotShort => "the position is not short; nothing is sent".into(),
            StopReason::ShortAboveSpot => "the short exceeds the MON held; nothing is sent".into(),
            StopReason::NoSpotToHedge => "no unhedged MON is left for the clip".into(),
            StopReason::PostsUsed => "the posts are used up; if the resting order later fills, run round.sh cancel (as the keeper) before any forceexit".into(),
            StopReason::LowGas => "the keeper's MON is under the stop floor; fund it".into(),
        }
    }
}

const BPS: u128 = 10_000;
const PPM: u128 = 1_000_000;
/// The Kuru touch is USDC per MON scaled by 1e18; USDC has 6 decimals, so
/// dividing by 1e12 gives CNS per MON, the unit of a Perpl price on perp 10.
const KURU_TO_CNS: u128 = 1_000_000_000_000;

pub fn decide(s: &State, p: &Params) -> Decision {
    let mut signals = Vec::new();
    let action = decide_action(s, p, &mut signals);
    signals.retain(|k| due(s, p, *k));
    Decision { action, signals }
}

fn stop(reason: StopReason) -> Action {
    Action::Stop { reason }
}

fn wait(reason: WaitReason) -> Action {
    Action::Wait { reason }
}

fn decide_action(s: &State, p: &Params, signals: &mut Vec<SignalKind>) -> Action {
    if let Some(reason) = record_behind_chain(s) {
        return stop(reason);
    }
    let pos = &s.position;
    if pos.lots > 0 && !pos.short {
        return stop(StopReason::NotShort);
    }
    if pos.lots > s.instance_mon_lots {
        return stop(StopReason::ShortAboveSpot);
    }
    if s.keeper_wei < p.gas_stop_wei {
        return stop(StopReason::LowGas);
    }
    if s.keeper_wei < p.gas_signal_wei {
        signals.push(SignalKind::LowGas);
    }
    if pos.lots > 0 {
        position_signals(s, p, signals);
        if let Some(amount_cns) = top_up(s, p, signals) {
            return Action::TopUp { amount_cns };
        }
    }

    let base = s.clip.base_lots.unwrap_or(pos.lots);
    let target = base + p.clip_lots;
    if s.resting_hedge != 0 {
        return manage_order(s, p, target);
    }
    if pos.lots < target {
        return post(s, p, target);
    }
    if pos.lots > 0 && !pos.mark_valid {
        return wait(WaitReason::MarkInvalid);
    }
    wait(WaitReason::Holding)
}

/// The record is the keeper's memory, so it must agree with the chain before
/// it is used: a crash between a mined transaction and its line would
/// otherwise miscount posts or post twice.
fn record_behind_chain(s: &State) -> Option<StopReason> {
    let behind = || {
        Some(StopReason::RecordBehindChain {
            order_id: s.resting_hedge,
            block: s.block,
        })
    };
    match &s.clip.last_post {
        None => (s.resting_hedge != 0).then(behind).flatten(),
        Some(lp) => {
            let order_agrees = if lp.cancelled {
                s.resting_hedge == 0
            } else {
                s.resting_hedge == lp.order_id
            };
            let lots = s.position.lots;
            let lots_agree =
                lots >= lp.position_lots_before && lots <= lp.position_lots_before + lp.lots;
            (!(order_agrees && lots_agree)).then(behind).flatten()
        }
    }
}

fn manage_order(s: &State, p: &Params, target: u64) -> Action {
    let order_id = s.resting_hedge;
    let cancel = |reason| Action::Cancel { order_id, reason };
    match &s.order {
        Some(o) if o.ours => {
            let posted = s.clip.last_post.as_ref().map_or(0, |lp| lp.time_sec);
            if s.time_sec.saturating_sub(posted) < p.requote_sec {
                wait(WaitReason::Resting)
            } else if s.clip.mined_posts >= p.max_posts {
                // No post would follow the cancel, so the last order rests.
                stop(StopReason::PostsUsed)
            } else {
                cancel(CancelReason::Requote)
            }
        }
        Some(_) | None if s.position.lots >= target => cancel(CancelReason::Filled),
        Some(_) | None => cancel(CancelReason::OrderGone),
    }
}

fn post(s: &State, p: &Params, target: u64) -> Action {
    if s.clip.mined_posts >= p.max_posts {
        return stop(StopReason::PostsUsed);
    }
    let lots = (target - s.position.lots).min(s.instance_mon_lots - s.position.lots);
    if lots == 0 {
        return stop(StopReason::NoSpotToHedge);
    }
    let perp = &s.perp;
    if perp.ign_oracle || perp.oracle_pns == 0 {
        return wait(WaitReason::OracleIgnored);
    }
    if perp.oracle_age_sec > p.oracle_age_to_sign_sec {
        return wait(WaitReason::OracleStale);
    }
    if perp.ask_pns <= perp.bid_pns + 1 {
        return wait(WaitReason::NoSpread);
    }
    let price_pns = perp.ask_pns - 1;
    if price_pns < floor_pns(perp.oracle_pns, p.discount_bps) {
        return wait(WaitReason::BelowFloor);
    }
    Action::Post { lots, price_pns }
}

/// The lowest price `hedge` accepts: `pricePNS × 10,000 ≥ oraclePNS ×
/// (10,000 − discount)`, so the ceiling of the quotient.
pub fn floor_pns(oracle_pns: u64, discount_bps: u64) -> u64 {
    let num = u128::from(oracle_pns) * BPS.saturating_sub(u128::from(discount_bps));
    u64::try_from(num.div_ceil(BPS)).expect("the floor is at most the oracle, which is a u64")
}

fn mark_at_least(pos: &Position, ratio_bps: u64) -> bool {
    u128::from(pos.mark_pns) * BPS >= u128::from(pos.entry_pns) * u128::from(ratio_bps)
}

fn position_signals(s: &State, p: &Params, signals: &mut Vec<SignalKind>) {
    let pos = &s.position;
    if pos.mark_valid && mark_at_least(pos, p.mark_rise_signal_bps) {
        signals.push(SignalKind::MarkRise);
    }
    let notional = u128::from(pos.lots) * u128::from(pos.entry_pns);
    let paid = pos.premium_pnl_cns.min(0).unsigned_abs();
    if paid * BPS >= u128::from(p.funding_signal_bps) * notional && paid > 0 {
        signals.push(SignalKind::Funding);
    }
    if exit_edge(s, p) {
        signals.push(SignalKind::ExitEdge);
    }
}

/// The touch exit edge (Kuru bid against the Perpl ask, net of the taker
/// fee) is at least 0, and the forceexit guard holds: a usable oracle no
/// older than the signing age, with the Kuru bid within the guard under it.
/// The bid is rounded down, so both checks are at least as strict as exact.
fn exit_edge(s: &State, p: &Params) -> bool {
    let perp = &s.perp;
    let bid = s.kuru.bid_e18 / KURU_TO_CNS;
    let ask = u128::from(perp.ask_pns);
    let oracle = u128::from(perp.oracle_pns);
    if perp.ign_oracle || oracle == 0 || perp.oracle_age_sec > p.oracle_age_to_sign_sec || ask == 0
    {
        return false;
    }
    let edge = bid >= ask && (bid - ask) * PPM >= u128::from(perp.taker_fee_ppm) * ask;
    let guard = oracle.saturating_sub(bid) * BPS <= u128::from(p.exit_guard_bps) * oracle;
    edge && guard
}

/// At the top-up threshold, the shortfall of the deposit from the notional at
/// the mark, capped at the free balance; with nothing free, a margin signal.
fn top_up(s: &State, p: &Params, signals: &mut Vec<SignalKind>) -> Option<u128> {
    let pos = &s.position;
    if !pos.mark_valid || !mark_at_least(pos, p.top_up_mark_bps) {
        return None;
    }
    let at_mark = u128::from(pos.lots) * u128::from(pos.mark_pns);
    let shortfall = at_mark.saturating_sub(pos.deposit_cns);
    if shortfall == 0 {
        return None;
    }
    let free = s.account.balance_cns.saturating_sub(s.account.locked_cns);
    if free == 0 {
        signals.push(SignalKind::Margin);
        return None;
    }
    Some(shortfall.min(free))
}

fn due(s: &State, p: &Params, kind: SignalKind) -> bool {
    s.clip
        .signals_fired_sec
        .get(&kind)
        .is_none_or(|&t| s.time_sec.saturating_sub(t) >= p.signal_repeat_sec)
}

#[cfg(test)]
pub(crate) mod tests;
