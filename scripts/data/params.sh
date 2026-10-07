# Parameters for fetch.sh and stats.sh. Sourced, not executed.
# Addresses are Monad mainnet (chain 143); each was confirmed with `cast code`.

RPC_URL="${MONAD_RPC_URL:-https://rpc.monad.xyz}"
CHAIN_ID=143

PERPL_EXCHANGE=0x34B6552d57a35a1D042CcAe1951BD1C370112a6F

# Perp id, Kuru book against USDC, label. MON's book has native MON as base.
MARKETS=(
    "1 0x40c49F171202F91ff5d2faE34c22dD2BFdD22aF0 BTC"
    "20 0xa6aFD386135B7D41A6C40C525abC4A1019b0D132 ETH"
    "10 0x065C9d28E428A0db40191a54d33d5b7c71a9C394 MON"
)

# Perpl applies funding at block multiples of this interval, and emits
# FundingEventCompleted up to 143 blocks before the event block.
FUNDING_INTERVAL_BLOCKS=8571
FUNDING_EMIT_LOOKBACK_BLOCKS=199
# About 31 days at 2,593 s an interval.
FUNDING_INTERVALS=1060

# Book samples: one block every BOOK_STEP_BLOCKS, back as far as the RPC
# serves state (about 1,000,000 blocks); older samples come back as errors.
BOOK_STEP_BLOCKS=1000
BOOK_SAMPLES=1000

# Maker fills on one perp, over FILL_WINDOW_BLOCKS (about 3 days at 0.30 s).
FILL_PERP_ID=10
FILL_WINDOW_BLOCKS=864000
# eth_getLogs on the public RPC is limited to 100 blocks.
LOG_RANGE_BLOCKS=100
# Block timestamps are stored every BLOCK_TIME_STEP blocks across the fill window.
BLOCK_TIME_STEP=1000

# Requests per JSON-RPC batch, one batch a second: the public RPC allows 50
# requests a second and counts each call inside a batch.
BATCH_CALLS=40
BATCH_LOGS=20

# Event topics: keccak256 of the signatures in upstream's Exchange ABI.
TOPIC_FUNDING_EVENT_COMPLETED=0x9c0e05c2bbc786a75afe5fed63ff9274f7c690af8ea4e843e07f2a525295c104
TOPIC_MAKER_ORDER_FILLED_V2=0xa59d6df87b5cb9e8cca8c09e8f1e240b7a1d4a2ee8f6c636c12ce22b43b82d70
TOPIC_MAKER_ORDER_FILLED=0xf5f5aef063f495816b6982c44c70c97c8fbec2fa24b73487e6542ce021431214
TOPIC_ERC20_TRANSFER=0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef
