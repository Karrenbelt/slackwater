//! Cases from `data/raw/rounds.jsonl` (round 1) and from the branches
//! `round.sh` has that mainnet has not run.

use super::*;

pub(crate) fn state_at_clip_1_post() -> State {
    fresh()
}

const T: u64 = 1_000_000;
const MON: u128 = 1_000_000_000_000_000_000;

fn params() -> Params {
    Params {
        clip_lots: 1850,
        max_posts: 4,
        requote_sec: 15 * 60,
        oracle_age_to_sign_sec: 40,
        discount_bps: 50,
        top_up_mark_bps: 12_500,
        mark_rise_signal_bps: 15_000,
        funding_signal_bps: 15,
        exit_guard_bps: 30,
        signal_repeat_sec: 30 * 60,
        gas_signal_wei: 5 * MON,
        gas_stop_wei: MON,
    }
}

/// Clip 1's post read (`hedge-1`, block 111,602,977), on a clip with no post
/// yet and no position.
fn fresh() -> State {
    State {
        block: 111_602_977,
        time_sec: T,
        perp: PerpView {
            oracle_pns: 25_361,
            oracle_age_sec: 21,
            ign_oracle: false,
            bid_pns: 25_340,
            ask_pns: 25_384,
            taker_fee_ppm: 345,
            perp_mark_pns: 25_369,
            funding_rate_pct100k: 0,
        },
        kuru: KuruTouch {
            bid_e18: 25_360_000_000_000_000,
            ask_e18: 25_391_000_000_000_000,
        },
        account: Account {
            balance_cns: 110_000_000,
            locked_cns: 0,
        },
        position: Position {
            short: false,
            lots: 0,
            deposit_cns: 0,
            entry_pns: 0,
            premium_pnl_cns: 0,
            mark_pns: 25_369,
            mark_valid: true,
        },
        resting_hedge: 0,
        order: None,
        instance_mon_lots: 1850,
        keeper_wei: 188 * MON,
        clip: ClipMemory::default(),
    }
}

fn short(lots: u64) -> Position {
    Position {
        short: lots > 0,
        lots,
        deposit_cns: u128::from(lots) * 25_383,
        entry_pns: 25_383,
        premium_pnl_cns: 0,
        mark_pns: 25_383,
        mark_valid: true,
    }
}

/// Order 11 posted `age_sec` ago for the whole clip at 25,383, with `filled`
/// lots of it filled.
fn resting(age_sec: u64, filled: u64, ours: bool) -> State {
    let mut s = fresh();
    s.resting_hedge = 11;
    s.order = Some(RestingOrder {
        order_id: 11,
        ours,
        lots: 1850 - filled,
        price_pns: 25_383,
    });
    s.position = short(filled);
    s.clip = ClipMemory {
        base_lots: Some(0),
        mined_posts: 1,
        last_post: Some(LastPost {
            order_id: 11,
            lots: 1850,
            position_lots_before: 0,
            block: 111_603_005,
            time_sec: T - age_sec,
            cancelled: false,
        }),
        signals_fired_sec: BTreeMap::new(),
    };
    s
}

/// The clip filled whole and its record cleared (`cancel-final`).
fn holding() -> State {
    let mut s = resting(60, 1850, false);
    s.resting_hedge = 0;
    s.order = None;
    if let Some(lp) = s.clip.last_post.as_mut() {
        lp.cancelled = true;
    }
    s.account = Account {
        balance_cns: 63_039_336,
        locked_cns: 0,
    };
    s
}

fn action(s: &State) -> Action {
    decide(s, &params()).action
}

fn waits(reason: WaitReason) -> Action {
    Action::Wait { reason }
}

fn stops(reason: StopReason) -> Action {
    Action::Stop { reason }
}

#[test]
fn clip_1_post_read_posts_one_tick_under_the_ask() {
    assert_eq!(floor_pns(25_361, 50), 25_235);
    assert_eq!(
        action(&fresh()),
        Action::Post {
            lots: 1850,
            price_pns: 25_383
        }
    );
}

#[test]
fn sign_rules_wait_with_their_reason() {
    let mut s = fresh();
    s.perp.oracle_age_sec = 41;
    assert_eq!(action(&s), waits(WaitReason::OracleStale));

    let mut s = fresh();
    s.perp.ign_oracle = true;
    assert_eq!(action(&s), waits(WaitReason::OracleIgnored));

    let mut s = fresh();
    s.perp.bid_pns = 25_383;
    assert_eq!(action(&s), waits(WaitReason::NoSpread));

    let mut s = fresh();
    s.perp.oracle_pns = 25_700;
    assert_eq!(action(&s), waits(WaitReason::BelowFloor));
}

#[test]
fn requote_after_fifteen_minutes_part_filled_or_not() {
    assert_eq!(
        action(&resting(14 * 60, 0, true)),
        waits(WaitReason::Resting)
    );
    let requote = Action::Cancel {
        order_id: 11,
        reason: CancelReason::Requote,
    };
    assert_eq!(action(&resting(15 * 60, 0, true)), requote);
    assert_eq!(action(&resting(15 * 60, 600, true)), requote);

    let mut s = resting(15 * 60, 600, true);
    s.resting_hedge = 0;
    s.order = None;
    if let Some(lp) = s.clip.last_post.as_mut() {
        lp.cancelled = true;
    }
    assert_eq!(
        action(&s),
        Action::Post {
            lots: 1250,
            price_pns: 25_383
        }
    );
}

fn behind(order_id: u64) -> Action {
    stops(StopReason::RecordBehindChain {
        order_id,
        block: 111_602_977,
    })
}

#[test]
fn record_behind_chain_stops_naming_the_order() {
    let mut s = fresh();
    s.resting_hedge = 11;
    assert_eq!(action(&s), behind(11), "a hedge with no hedge- line");

    let mut s = resting(60, 0, true);
    s.resting_hedge = 12;
    assert_eq!(
        action(&s),
        behind(12),
        "a hedge the last line does not name"
    );

    let mut s = resting(60, 0, true);
    s.resting_hedge = 0;
    s.order = None;
    assert_eq!(action(&s), behind(0), "a cleared hedge with no cancel line");

    let mut s = resting(60, 0, true);
    s.position = short(1900);
    assert_eq!(action(&s), behind(11), "more filled than posted");
}

#[test]
fn position_below_the_last_posts_base_stops() {
    // A second post on 600 filled lots, then the position falls to 500: a
    // liquidation or an auto-deleverage the record cannot explain.
    let mut s = resting(60, 600, true);
    if let Some(lp) = s.clip.last_post.as_mut() {
        lp.position_lots_before = 600;
        lp.lots = 1250;
    }
    s.position = short(500);
    assert_eq!(action(&s), behind(11));
}

#[test]
fn invalid_mark_waits_and_signals_nothing() {
    let mut s = holding();
    s.position.mark_pns = 25_383 * 16 / 10;
    s.position.mark_valid = false;
    let d = decide(&s, &params());
    assert_eq!(d.action, waits(WaitReason::MarkInvalid));
    assert!(d.signals.is_empty(), "{:?}", d.signals);
}

#[test]
fn gas_floor_signals_then_stops() {
    let mut s = holding();
    s.keeper_wei = 49 * MON / 10;
    let d = decide(&s, &params());
    assert_eq!(d.action, waits(WaitReason::Holding));
    assert_eq!(d.signals, vec![SignalKind::LowGas]);

    s.keeper_wei = 9 * MON / 10;
    assert_eq!(action(&s), stops(StopReason::LowGas));
}

#[test]
fn order_no_longer_ours_is_cleared() {
    assert_eq!(
        action(&resting(60, 600, false)),
        Action::Cancel {
            order_id: 11,
            reason: CancelReason::OrderGone
        }
    );
    assert_eq!(
        action(&resting(60, 1850, false)),
        Action::Cancel {
            order_id: 11,
            reason: CancelReason::Filled
        }
    );
}

#[test]
fn posts_used_and_short_above_spot_stop() {
    let mut s = resting(15 * 60, 600, true);
    s.resting_hedge = 0;
    s.order = None;
    s.clip.mined_posts = 4;
    if let Some(lp) = s.clip.last_post.as_mut() {
        lp.cancelled = true;
    }
    assert_eq!(action(&s), stops(StopReason::PostsUsed));

    // The fourth post due for a requote rests instead of being cancelled.
    let mut s = resting(15 * 60, 600, true);
    s.clip.mined_posts = 4;
    assert_eq!(action(&s), stops(StopReason::PostsUsed));

    let mut s = resting(60, 0, true);
    if let Some(lp) = s.clip.last_post.as_mut() {
        lp.lots = 1900;
    }
    s.position = short(1900);
    assert_eq!(action(&s), stops(StopReason::ShortAboveSpot));
}

#[test]
fn top_up_at_the_threshold_capped_at_the_free_balance() {
    let mut s = holding();
    s.position.mark_pns = 31_729; // 25,383 × 1.25, rounded up
    // 1,850 × 31,729 − 46,958,550
    assert_eq!(
        action(&s),
        Action::TopUp {
            amount_cns: 11_740_100
        }
    );

    s.account = Account {
        balance_cns: 50_000_000,
        locked_cns: 45_000_000,
    };
    assert_eq!(
        action(&s),
        Action::TopUp {
            amount_cns: 5_000_000
        }
    );

    s.account.locked_cns = 50_000_000;
    let d = decide(&s, &params());
    assert_eq!(d.action, waits(WaitReason::Holding));
    assert_eq!(d.signals, vec![SignalKind::Margin]);
}

#[test]
fn mark_rise_and_funding_signals() {
    let mut s = holding();
    s.position.mark_pns = 25_383 * 3 / 2 + 1;
    assert!(
        decide(&s, &params())
            .signals
            .contains(&SignalKind::MarkRise)
    );

    // 15 bps of 46,958,550 is 70,437.8.
    let mut s = holding();
    s.position.premium_pnl_cns = -70_438;
    assert_eq!(decide(&s, &params()).signals, vec![SignalKind::Funding]);
    s.position.premium_pnl_cns = -65_741;
    assert!(decide(&s, &params()).signals.is_empty());
}

fn edge_state(oracle: u64, ask: u64, kuru_bid_cns: u128) -> State {
    let mut s = holding();
    s.perp.oracle_pns = oracle;
    s.perp.ask_pns = ask;
    s.perp.oracle_age_sec = 7;
    s.kuru.bid_e18 = kuru_bid_cns * 1_000_000_000_000;
    s
}

#[test]
fn exit_edge_needs_the_edge_and_the_guard() {
    // The signed exit's read, block 112,173,924: touch edge −33.8 bps.
    let s = edge_state(24_673, 24_727, 24_652);
    assert!(decide(&s, &params()).signals.is_empty());
    // Edge +1.0 bps, Kuru bid 3.7 bps under the oracle.
    let s = edge_state(24_620, 24_600, 24_611);
    assert_eq!(decide(&s, &params()).signals, vec![SignalKind::ExitEdge]);
    // The same edge with the bid 31.2 bps under the oracle.
    let s = edge_state(24_688, 24_600, 24_611);
    assert!(decide(&s, &params()).signals.is_empty());
}

#[test]
fn a_signal_repeats_after_thirty_minutes() {
    let mut s = edge_state(24_620, 24_600, 24_611);
    s.clip
        .signals_fired_sec
        .insert(SignalKind::ExitEdge, T - 29 * 60);
    assert!(decide(&s, &params()).signals.is_empty());
    s.clip
        .signals_fired_sec
        .insert(SignalKind::ExitEdge, T - 30 * 60);
    assert_eq!(decide(&s, &params()).signals, vec![SignalKind::ExitEdge]);
}
