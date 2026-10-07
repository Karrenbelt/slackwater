//! Stored rows of `data/raw/` and their typed decoding.

use std::path::Path;

use alloy::{
    primitives::{Bytes, U64, U256},
    sol_types::{SolCall, SolEvent},
};
use anyhow::Context;
use perpl_sdk::abi::dex::Exchange::{
    FundingEventCompleted, PerpetualInfoV2, getPerpetualInfoV2Call, getTakerFeeCall,
};
use serde::{Deserialize, Deserializer, de::DeserializeOwned};

use crate::bindings::KuruOrderBook::bestBidAskCall;

#[derive(Debug, thiserror::Error)]
pub enum RowError {
    #[error("{at}: {what} is an error row: {error}")]
    ErrorRow {
        at: String,
        what: &'static str,
        error: serde_json::Value,
    },
    #[error("{at}: {what} has no result")]
    Missing { at: String, what: &'static str },
    #[error("{at}: {what} does not decode: {source}")]
    Decode {
        at: String,
        what: &'static str,
        source: Box<alloy::sol_types::Error>,
    },
}

/// Block numbers and log indices are JSON numbers in some files and hex
/// strings in others.
fn number_or_hex<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Number(u64),
        Hex(U64),
    }
    Ok(match Raw::deserialize(d)? {
        Raw::Number(n) => n,
        Raw::Hex(h) => h.to(),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Block,
    Perpl,
    Kuru,
}

/// One reading of `book-samples.jsonl`: a block timestamp (not read here), a
/// Perpl `getPerpetualInfoV2` result or a Kuru `bestBidAsk` result.
#[derive(Debug, Deserialize)]
pub struct BookRow {
    #[serde(deserialize_with = "number_or_hex")]
    pub block: u64,
    pub source: Source,
    pub perp_id: Option<u32>,
    pub result: Option<Bytes>,
    pub error: Option<serde_json::Value>,
}

/// One row of `funding-events.jsonl`: a `FundingEventCompleted` log, or an
/// error row for a log request that failed.
#[derive(Debug, Deserialize)]
pub struct FundingRow {
    #[serde(default, deserialize_with = "optional_number_or_hex")]
    pub block: Option<u64>,
    #[serde(default, deserialize_with = "optional_number_or_hex")]
    pub log_index: Option<u64>,
    pub data: Option<Bytes>,
    pub interval_index: Option<u64>,
    pub error: Option<serde_json::Value>,
}

fn optional_number_or_hex<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    number_or_hex(d).map(Some)
}

#[derive(Debug, Deserialize)]
pub struct FeeRow {
    pub block: u64,
    #[serde(rename = "fn")]
    pub function: String,
    pub perp_id: u32,
    pub result: Option<Bytes>,
    pub error: Option<serde_json::Value>,
}

/// A row of `maker-fills.jsonl`, whose data words are stored with leading
/// zeros trimmed and are not decoded here.
#[derive(Debug, Deserialize)]
pub struct MakerFillRow {
    #[serde(deserialize_with = "number_or_hex")]
    pub block: u64,
    pub tx: alloy::primitives::TxHash,
    #[serde(deserialize_with = "number_or_hex")]
    pub log_index: u64,
}

#[derive(Debug, Deserialize)]
pub struct BlockTimeRow {
    #[serde(deserialize_with = "number_or_hex")]
    pub block: u64,
    pub timestamp: Option<U64>,
}

pub fn read_jsonl<T: DeserializeOwned>(path: &Path) -> anyhow::Result<Vec<T>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(i, line)| {
            serde_json::from_str(line)
                .with_context(|| format!("{}:{}: row does not fit", path.display(), i + 1))
        })
        .collect()
}

fn result_of<'a>(
    at: &str,
    what: &'static str,
    result: &'a Option<Bytes>,
    error: &Option<serde_json::Value>,
) -> Result<&'a [u8], RowError> {
    match (result, error) {
        (_, Some(error)) => Err(RowError::ErrorRow {
            at: at.to_owned(),
            what,
            error: error.clone(),
        }),
        (Some(bytes), None) => Ok(bytes),
        (None, None) => Err(RowError::Missing {
            at: at.to_owned(),
            what,
        }),
    }
}

fn decode_error(at: &str, what: &'static str) -> impl FnOnce(alloy::sol_types::Error) -> RowError {
    move |source| RowError::Decode {
        at: at.to_owned(),
        what,
        source: Box::new(source),
    }
}

pub fn decode_perpl(row: &BookRow) -> Result<PerpetualInfoV2, RowError> {
    let (at, what) = (format!("block {}", row.block), "getPerpetualInfoV2");
    let bytes = result_of(&at, what, &row.result, &row.error)?;
    getPerpetualInfoV2Call::abi_decode_returns(bytes).map_err(decode_error(&at, what))
}

/// Kuru's best bid and ask, scaled by 1e18. An empty side is returned by the
/// book as 0 or as the largest uint256, and decodes as `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KuruTouch {
    pub bid: Option<U256>,
    pub ask: Option<U256>,
}

pub fn decode_kuru(row: &BookRow) -> Result<KuruTouch, RowError> {
    let (at, what) = (format!("block {}", row.block), "bestBidAsk");
    let bytes = result_of(&at, what, &row.result, &row.error)?;
    let quote = bestBidAskCall::abi_decode_returns(bytes).map_err(decode_error(&at, what))?;
    let side = |v: U256| (!v.is_zero() && v != U256::MAX).then_some(v);
    Ok(KuruTouch {
        bid: side(quote._0),
        ask: side(quote._1),
    })
}

pub fn decode_funding(row: &FundingRow) -> Result<FundingEventCompleted, RowError> {
    let what = "FundingEventCompleted";
    let at = match (row.block, row.interval_index) {
        (Some(block), _) => format!("block {block}"),
        (None, Some(interval)) => format!("interval {interval}"),
        (None, None) => "a row with no block or interval".to_owned(),
    };
    let bytes = result_of(&at, what, &row.data, &row.error)?;
    FundingEventCompleted::decode_raw_log([FundingEventCompleted::SIGNATURE_HASH], bytes)
        .map_err(decode_error(&at, what))
}

pub fn decode_fee(row: &FeeRow) -> Result<U256, RowError> {
    let (at, what) = (format!("block {}", row.block), "getTakerFee");
    let bytes = result_of(&at, what, &row.result, &row.error)?;
    getTakerFeeCall::abi_decode_returns(bytes).map_err(decode_error(&at, what))
}

#[cfg(test)]
mod tests {
    use alloy::{primitives::U256, sol_types::SolValue};

    use super::*;

    fn book_rows() -> Vec<BookRow> {
        include_str!("../fixtures/book-samples-111024000.jsonl")
            .lines()
            .map(|l| serde_json::from_str(l).expect("fixture row is a BookRow"))
            .collect()
    }

    fn row(source: Source, perp_id: u32) -> BookRow {
        book_rows()
            .into_iter()
            .find(|r| r.source == source && r.perp_id == Some(perp_id))
            .expect("fixture holds every market at block 111,024,000")
    }

    #[test]
    fn btc_perpetual_info_at_111024000() {
        let info = decode_perpl(&row(Source::Perpl, 1)).expect("decodes");
        assert_eq!(info.priceDecimals, U256::from(1));
        assert_eq!(info.markPNS, U256::from(863_258));
        assert_eq!(info.oraclePNS, U256::from(863_027));
        assert_eq!(info.maxBidPriceONS, U256::from(863_345));
        assert_eq!(info.minAskPriceONS, U256::from(863_346));
    }

    #[test]
    fn eth_funding_event_111020163() {
        let line = include_str!("../fixtures/funding-event-eth-111020163.jsonl");
        let row: FundingRow = serde_json::from_str(line).expect("fixture row is a FundingRow");
        let event = decode_funding(&row).expect("decodes");
        assert_eq!(event.perpId, U256::from(20));
        assert_eq!(event.fundingEventBlock, U256::from(111_020_163));
        assert_eq!(event.actualRatePct100k.as_i64(), -4);
        assert_eq!(event.fundingPricePNS, U256::from(271_329));
        assert_eq!(event.fundingPaymentPNS.as_i64(), -10);
        assert_eq!(event.fundingSumPNS.as_i64(), 1_342);
    }

    #[test]
    fn kuru_quote_at_111024000_has_both_sides() {
        let touch = decode_kuru(&row(Source::Kuru, 1)).expect("decodes");
        assert!(touch.bid.is_some() && touch.ask.is_some());
    }

    #[test]
    fn kuru_empty_side_is_no_quote() {
        let encoded = (U256::ZERO, U256::MAX).abi_encode_params();
        let row = BookRow {
            block: 1,
            source: Source::Kuru,
            perp_id: Some(10),
            result: Some(encoded.into()),
            error: None,
        };
        assert_eq!(
            decode_kuru(&row).expect("decodes"),
            KuruTouch {
                bid: None,
                ask: None
            }
        );
    }

    #[test]
    fn error_row_is_an_error_not_a_value() {
        let row: BookRow = serde_json::from_str(
            r#"{"block":5,"source":"perpl","perp_id":1,"error":{"code":-32000,"message":"x"}}"#,
        )
        .expect("parses");
        assert!(matches!(decode_perpl(&row), Err(RowError::ErrorRow { .. })));
    }
}
