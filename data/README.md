# Data

Chain readings from Monad mainnet (chain 143), stored so that every figure we quote can be re-derived without trusting a summary.

- `raw/`: what the RPC returned, written by `scripts/data/fetch.sh` and `slackwater fetch`.
  Every row carries the block it was read at.
- `derived/`: statistics computed from `raw/` alone, with no network access, by `scripts/data/stats.sh` and the `slackwater check` commands.
  It is not committed; regenerate it with the commands below.
- `MANIFEST.jsonl`: one row per dataset, with the block range, the RPC, the git commit and the SHA-256 of what made it.
  A new fetch appends a row; the latest row for a dataset describes its current file.

The script hashes in the manifest do not match the committed scripts: the books, fees and funding rows were made by an uncommitted `fetch.sh` (SHA-256 `8570055d...`) that differed from the later one only in how it writes `maker-fills.jsonl`, and comments in `fetch.sh`, `params.sh` and `stats.sh` were edited before the first commit.

Which files could be fetched again:
- `raw/book-samples.jsonl` cannot: the public RPC serves state only about 1,000,000 blocks back, so this file is the only copy.
- The funding and fill logs can, while the RPC still serves logs that far back (at least 9,000,000 blocks), but `fetch.sh` always fetches back from the latest block, so a new run covers a newer window than the stored rows.
- `raw/fill-tx-logs.jsonl` (about 80 MB) is not committed: `slackwater fetch fill-txs` fetches it again exactly, from the transactions listed in `raw/maker-fills.jsonl`.

| File | Contents |
|---|---|
| `raw/funding-events.jsonl` | Perpl `FundingEventCompleted` logs for perps 1 (BTC), 20 (ETH) and 10 (MON), about 31 days |
| `raw/funding-blocks.jsonl` | Timestamps of the blocks that emitted them |
| `raw/book-samples.jsonl` | Every 1,000 blocks for about 3.4 days, at the same block: Perpl `getPerpetualInfoV2` (raw ABI result) and Kuru `bestBidAsk` for the BTC, ETH and MON books against USDC |
| `raw/maker-fills.jsonl` | Perpl `MakerOrderFilled` and `MakerOrderFilledV2` logs on perp 10 (MON), about 3 days, as data words with leading zeros trimmed |
| `raw/block-times.jsonl` | Block timestamps every 1,000 blocks across the fill window |
| `raw/fees.jsonl` | Perpl taker and maker fee per market, in ppm |
| `raw/fill-tx-logs.jsonl` | Request, fill and batch-completed logs from the receipts of the transactions in `raw/maker-fills.jsonl` (not committed) |
| `raw/rounds.jsonl` | Every transaction and every named read of the mainnet rounds of our `BasisTrader` instance, one line each, in order |

`raw/rounds.jsonl` is append-only, and a line is written right after the step it records.
Every line carries `round`, `clip` and `step`.
A line for a transaction also carries `tx`, `block`, `status`, `gasUsed`, `gasLimit`, `effectiveGasPriceWei` and `logs` (each log's `address`, `topic0` and `data`, as in the receipt), plus fields for that step.
A line for a read carries only the fields of that read, and every read names its block.
Prices are in the venue's own units: Perpl in PNS (6 decimals on MON), Kuru `bestBidAsk` in USDC per MON scaled by 1e18.
Amounts larger than 2^53 are strings.
The fee of a transaction is `gasUsed` × `effectiveGasPriceWei`; it is not stored.
The `step` names:
- transactions: `bridge-ausd`, `bridge-mon`, `deploy-<function>`, `fund-mon`, `hedge-<k>`, `cancel-<k>`, `cancel-final`;
- reads: `deploy-readback`, `acct-before-post`, `acct-after-post`, `acct-after-fill`, `fill`, `order-gone`, `exit-sim`.

A Kuru touch is state, not an event, so the Kuru reads in these lines cannot be fetched again once the RPC no longer serves their blocks.

Reproduce, from the repository root:

```sh
just data-stats                                    # offline: derived/funding.json, price-moves.json, books.json, fills.json
crates/slackwater/target/debug/slackwater check hedged-exit   # offline: derived/hedged-exit.json
crates/slackwater/target/debug/slackwater fetch fill-txs      # network: raw/fill-tx-logs.jsonl
crates/slackwater/target/debug/slackwater check fills         # derived/fills-taker-side.json
crates/slackwater/target/debug/slackwater check depth         # network, latest block: derived/depth-10.json
just data-fetch all                                # network; fetches a new window back from the latest block
```

Build the binary first with `just build-rs`.

Limits stated with the data:
- Book samples are about 5 minutes apart and read the top of each book only; shorter openings are not in them.
- Fill times are interpolated between stored block timestamps.
- The public RPC serves state about 1,000,000 blocks back and logs at least 9,000,000; book samples cannot be re-fetched once they are older than that.
