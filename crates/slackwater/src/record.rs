//! `data/raw/rounds.jsonl`, in `scripts/round/round.sh`'s schema: the clip's
//! memory read from it, and the keeper's lines written to it.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
};

use alloy::rpc::types::TransactionReceipt;
use serde_json::{Map, Value, json};

use crate::keeper::{SignalKind, State, floor_pns};

#[derive(Debug, thiserror::Error)]
pub enum RecordError {
    #[error("reading {path}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path} line {line}: {source}")]
    Json {
        path: PathBuf,
        line: usize,
        source: serde_json::Error,
    },
    #[error("appending to {path}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path} line {line}: {step} has no usable {field}")]
    Field {
        path: PathBuf,
        line: usize,
        step: String,
        field: &'static str,
    },
}

/// What the record says about one clip. The post's time is not here: lines
/// written by `round.sh` carry only the block, so the chain gives its time.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClipRecord {
    pub base_lots: Option<u64>,
    pub mined_posts: usize,
    pub last_post: Option<RecordedPost>,
    pub signals_fired_sec: BTreeMap<SignalKind, u64>,
    /// Lines whose step starts with each prefix, reverted ones included, for
    /// the next step's index, as `round.sh`'s `next_index` counts them.
    pub lines: BTreeMap<&'static str, usize>,
}

/// The step prefixes the keeper numbers.
pub const NUMBERED: [&str; 3] = ["hedge-", "cancel-", "top-up-"];

impl ClipRecord {
    pub fn next_step(&self, prefix: &'static str) -> String {
        format!(
            "{prefix}{}",
            self.lines.get(prefix).copied().unwrap_or(0) + 1
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedPost {
    pub order_id: u64,
    pub lots: u64,
    pub position_lots_before: u64,
    pub block: u64,
    pub cancelled: bool,
}

pub fn read_clip(path: &Path, round: u32, clip: u32) -> Result<ClipRecord, RecordError> {
    let text = std::fs::read_to_string(path).map_err(|source| RecordError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    parse_clip(&text, path, round, clip)
}

fn parse_clip(text: &str, path: &Path, round: u32, clip: u32) -> Result<ClipRecord, RecordError> {
    let mut rec = ClipRecord::default();
    // A corrected copy repeats a transaction's line; count each once.
    let mut posts = BTreeSet::new();
    for (i, line) in text
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
    {
        let n = i + 1;
        let v: Value = serde_json::from_str(line).map_err(|source| RecordError::Json {
            path: path.to_path_buf(),
            line: n,
            source,
        })?;
        if v["round"].as_u64() != Some(u64::from(round))
            || v["clip"].as_u64() != Some(u64::from(clip))
        {
            continue;
        }
        let step = v["step"].as_str().ok_or_else(|| RecordError::Field {
            path: path.to_path_buf(),
            line: n,
            step: String::new(),
            field: "step",
        })?;
        let field = |field: &'static str| {
            v[field].as_u64().ok_or_else(|| RecordError::Field {
                path: path.to_path_buf(),
                line: n,
                step: step.to_string(),
                field,
            })
        };
        let mined = v["status"].as_u64() == Some(1);
        if let Some(prefix) = NUMBERED.into_iter().find(|p| step.starts_with(p)) {
            *rec.lines.entry(prefix).or_default() += 1;
        }
        if step.starts_with("hedge-") && mined {
            let before = field("positionLotsBefore")?;
            rec.base_lots.get_or_insert(before);
            if let Some(tx) = v["tx"].as_str() {
                posts.insert(tx.to_string());
            }
            rec.last_post = Some(RecordedPost {
                order_id: field("orderId")?,
                lots: field("lots")?,
                position_lots_before: before,
                block: field("block")?,
                cancelled: false,
            });
        } else if step.starts_with("cancel-") && mined {
            if let Some(lp) = rec.last_post.as_mut() {
                lp.cancelled = true;
            }
        } else if let Some(kind) = step.strip_prefix("signal-").and_then(SignalKind::from_name) {
            rec.signals_fired_sec.insert(kind, field("timeSec")?);
        }
    }
    rec.mined_posts = posts.len();
    Ok(rec)
}

/// The round and clip every line names, and the time of the read.
#[derive(Debug, Clone, Copy)]
pub struct Ctx {
    pub round: u32,
    pub clip: u32,
    pub time_sec: u64,
}

/// A mined transaction, as `round.sh`'s `rec` records it.
#[derive(Debug, Clone)]
pub struct Mined {
    pub tx: String,
    pub block: u64,
    pub status: u64,
    pub gas_used: u64,
    /// `None` when the transaction could not be read after its receipt.
    pub gas_limit: Option<u64>,
    pub effective_gas_price_wei: u128,
    pub logs: Vec<Value>,
}

impl Mined {
    pub fn from_receipt(r: &TransactionReceipt, gas_limit: Option<u64>) -> Option<Self> {
        Some(Mined {
            tx: format!("{:#x}", r.transaction_hash),
            block: r.block_number?,
            status: u64::from(r.status()),
            gas_used: r.gas_used,
            gas_limit,
            effective_gas_price_wei: r.effective_gas_price,
            logs: r
                .logs()
                .iter()
                .map(|l| {
                    json!({
                        "address": format!("{:#x}", l.inner.address),
                        "topic0": l.topic0().map(|t| format!("{t:#x}")),
                        "data": l.inner.data.data.to_string(),
                    })
                })
                .collect(),
        })
    }
}

fn head(ctx: Ctx, step: &str) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("round".into(), json!(ctx.round));
    m.insert("clip".into(), json!(ctx.clip));
    m.insert("step".into(), json!(step));
    m
}

fn tail(mut m: Map<String, Value>, ctx: Ctx, fields: Map<String, Value>) -> Value {
    m.extend(fields);
    m.insert("timeSec".into(), json!(ctx.time_sec));
    m.insert("by".into(), json!("keeper"));
    Value::Object(m)
}

/// A line for a mined transaction: `rec`'s fields, then `fields`.
pub fn tx_line(ctx: Ctx, step: &str, mined: &Mined, fields: Map<String, Value>) -> Value {
    let mut m = head(ctx, step);
    m.insert("tx".into(), json!(mined.tx));
    m.insert("block".into(), json!(mined.block));
    m.insert("status".into(), json!(mined.status));
    m.insert("gasUsed".into(), json!(mined.gas_used));
    m.insert("gasLimit".into(), json!(mined.gas_limit));
    m.insert(
        "effectiveGasPriceWei".into(),
        num(mined.effective_gas_price_wei),
    );
    m.insert("logs".into(), Value::Array(mined.logs.clone()));
    tail(m, ctx, fields)
}

/// A line for a read: `note`'s fields, then `fields`, which name the block.
pub fn read_line(ctx: Ctx, step: &str, fields: Map<String, Value>) -> Value {
    tail(head(ctx, step), ctx, fields)
}

/// `perp` as `round.sh`'s `perp` read prints it.
pub fn perp_fields(s: &State, discount_bps: u64) -> Value {
    json!({
        "block": s.block,
        "mark": s.perp.perp_mark_pns,
        "oracle": s.perp.oracle_pns,
        "oracleAgeSec": s.perp.oracle_age_sec,
        "bid": s.perp.bid_pns,
        "ask": s.perp.ask_pns,
        "ignOracle": s.perp.ign_oracle,
        "fundingRatePct100k": s.perp.funding_rate_pct100k,
        "floor": floor_pns(s.perp.oracle_pns, discount_bps),
    })
}

/// `kuru` as `round.sh` prints it: strings, as they exceed 2^53.
pub fn kuru_fields(s: &State) -> Value {
    json!({ "block": s.block, "bid": s.kuru.bid_e18.to_string(), "ask": s.kuru.ask_e18.to_string() })
}

/// An `acct-*` line's fields; `depositCNS` and `lotLNS` as `round.sh` now
/// writes them.
pub fn acct_fields(s: &State, account_id: u64) -> Map<String, Value> {
    fields([
        ("block", json!(s.block)),
        ("accountId", json!(account_id)),
        ("balanceCNS", num(s.account.balance_cns)),
        ("lockedBalanceCNS", num(s.account.locked_cns)),
        ("depositCNS", num(s.position.deposit_cns)),
        ("lotLNS", json!(s.position.lots)),
    ])
}

/// A `hedge-<k>` line's fields; the `requestId` is the block decided on.
pub fn hedge_fields(
    s: &State,
    discount_bps: u64,
    lots: u64,
    price_pns: u64,
    order_id: u64,
) -> Map<String, Value> {
    fields([
        ("perp", perp_fields(s, discount_bps)),
        ("kuru", kuru_fields(s)),
        ("lots", json!(lots)),
        ("pricePNS", json!(price_pns)),
        ("requestId", json!(s.block)),
        ("orderId", json!(order_id)),
        ("positionLotsBefore", json!(s.position.lots)),
    ])
}

/// The position as `round.sh`'s `pos` read prints it.
pub fn pos_fields(s: &State) -> Value {
    Value::Object(fields([
        ("block", json!(s.block)),
        ("positionType", json!(u8::from(s.position.short))),
        ("depositCNS", num(s.position.deposit_cns)),
        ("pricePNS", json!(s.position.entry_pns)),
        ("lotLNS", json!(s.position.lots)),
        (
            "premiumPnlCNS",
            json!(s.position.premium_pnl_cns.to_string()),
        ),
        ("markPNS", json!(s.position.mark_pns)),
        ("markValid", json!(s.position.mark_valid)),
    ]))
}

/// A `cancel-<k>` or `cancel-final` line's fields; `after` is read after the
/// receipt.
pub fn cancel_fields(
    order_id: u64,
    ours: bool,
    unfilled_lots: u64,
    after: &State,
) -> Map<String, Value> {
    fields([
        ("orderId", json!(order_id)),
        ("oursBeforeCancel", json!(ours)),
        ("unfilledLotLNS", json!(unfilled_lots)),
        ("positionAfter", pos_fields(after)),
    ])
}

pub fn fields<const N: usize>(kv: [(&str, Value); N]) -> Map<String, Value> {
    kv.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

/// A number up to 2^53, which a JSON reader holds exactly; above it a string.
pub fn num(v: u128) -> Value {
    const EXACT: u128 = 1 << 53;
    match u64::try_from(v) {
        Ok(n) if v <= EXACT => json!(n),
        Ok(_) | Err(_) => json!(v.to_string()),
    }
}

/// Appends one line with a single `write` on a file opened for append, so a
/// line is never interleaved with another writer's.
pub fn append(path: &Path, line: &Value) -> Result<(), RecordError> {
    let mut text = line.to_string();
    text.push('\n');
    OpenOptions::new()
        .append(true)
        .open(path)
        .and_then(|mut f| f.write_all(text.as_bytes()))
        .map_err(|source| RecordError::Write {
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    const ROUND_1: &str = include_str!("../../../data/raw/rounds.jsonl");

    #[test]
    fn round_1_clip_1_reads_one_cancelled_post() {
        let rec = parse_clip(ROUND_1, Path::new("rounds.jsonl"), 1, 1).expect("round 1 parses");
        assert_eq!(rec.base_lots, Some(0));
        assert_eq!(rec.mined_posts, 1);
        assert_eq!(
            rec.last_post,
            Some(RecordedPost {
                order_id: 11,
                lots: 1850,
                position_lots_before: 0,
                block: 111_603_005,
                cancelled: true,
            })
        );
    }

    #[test]
    fn clip_2_has_no_lines_yet() {
        let rec = parse_clip(ROUND_1, Path::new("rounds.jsonl"), 1, 2).expect("round 1 parses");
        assert_eq!(rec, ClipRecord::default());
    }

    fn round_1_line(step: &str) -> Value {
        ROUND_1
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .find(|v| v["step"] == step && v["clip"] == 1)
            .expect("round 1 has the line")
    }

    fn receipt() -> TransactionReceipt {
        serde_json::from_str(include_str!("../fixtures/receipt-hedge-1-111603005.json"))
            .expect("the fixture is a receipt")
    }

    #[test]
    fn hedge_line_reproduces_round_sh() {
        let theirs = round_1_line("hedge-1");
        let mined = Mined::from_receipt(&receipt(), Some(366_349)).expect("mined");
        let ctx = Ctx {
            round: 1,
            clip: 1,
            time_sec: 0,
        };
        let s = crate::keeper::tests::state_at_clip_1_post();
        let ours = tx_line(
            ctx,
            "hedge-1",
            &mined,
            hedge_fields(&s, 50, 1850, 25_383, 11),
        );
        for (k, v) in theirs.as_object().expect("an object") {
            match k.as_str() {
                // The read that round.sh recorded and the posted block's.
                "perp" | "kuru" | "requestId" => assert!(ours.get(k).is_some(), "{k}"),
                _ => assert_eq!(ours.get(k), Some(v), "{k}"),
            }
        }
        assert_eq!(ours["by"], "keeper");
    }

    #[test]
    fn synthetic_clip_2_for_reconcile() {
        // Written for `CLIP=2 round.sh reconcile`: a post, a fill of the
        // whole clip with a 2,114 CNS maker fee, cancel-final, acct-after-fill.
        let path = std::env::temp_dir().join("keeper-synthetic-clip-2.jsonl");
        std::fs::write(&path, ROUND_1).expect("temp record");
        let ctx = Ctx {
            round: 1,
            clip: 2,
            time_sec: 0,
        };
        let mut s = crate::keeper::tests::state_at_clip_1_post();
        s.account.balance_cns = 111_178_838;
        append(
            &path,
            &read_line(ctx, "acct-before-post", acct_fields(&s, 5427)),
        )
        .expect("line");
        let mined = Mined::from_receipt(&receipt(), Some(366_349)).expect("mined");
        append(
            &path,
            &tx_line(
                ctx,
                "hedge-1",
                &mined,
                hedge_fields(&s, 50, 1850, 25_383, 11),
            ),
        )
        .expect("line");
        let fill = round_1_line("fill");
        let mut fill = fill.as_object().expect("an object").clone();
        fill.insert("clip".into(), json!(2));
        append(&path, &Value::Object(fill)).expect("line");
        let mut after = s.clone();
        after.position.short = true;
        after.position.lots = 1850;
        after.position.deposit_cns = 46_958_550;
        after.account.balance_cns = 111_178_838 - 46_958_550 - 2_114;
        append(
            &path,
            &tx_line(
                ctx,
                "cancel-final",
                &mined,
                cancel_fields(11, false, 0, &after),
            ),
        )
        .expect("line");
        append(
            &path,
            &read_line(ctx, "acct-after-fill", acct_fields(&after, 5427)),
        )
        .expect("line");
        let rec = read_clip(&path, 1, 2).expect("reads back");
        assert_eq!(rec.mined_posts, 1);
        assert!(rec.last_post.is_some_and(|lp| lp.cancelled));
    }

    #[test]
    fn a_post_without_its_order_id_is_an_error() {
        let text = r#"{"round":1,"clip":2,"step":"hedge-1","tx":"0x1","status":1,"block":5,"lots":1850,"positionLotsBefore":0}"#;
        let err = parse_clip(text, Path::new("r"), 1, 2)
            .unwrap_err()
            .to_string();
        assert!(err.contains("orderId"), "{err}");
    }
}
