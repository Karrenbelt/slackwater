# Parameters for round.sh. Sourced, not executed.
# Every value can be overridden from the environment. Addresses are Monad
# mainnet (chain 143); each was confirmed with `cast code`.

RPC="${MONAD_RPC_URL:-https://rpc.monad.xyz}"

PERPL=0x34B6552d57a35a1D042CcAe1951BD1C370112a6F
AUSD=0x00000000eFE302BEAA2b3e6e1b18d08D69a9012a
# Kuru MON/USDC book; MON is its native base.
BOOK=0x065C9d28E428A0db40191a54d33d5b7c71a9C394
PERP_ID=10

# The owner signs deploy, funding and forced exits; the keeper signs hedge
# posts and cancels. Both sign with a Foundry keystore (`cast wallet list`).
OWNER="${OWNER:-0x92e4e69ea99c42337c3ea70a9b6aa1b6c91ba5e2}"
OWNER_ACCOUNT="${OWNER_ACCOUNT:-slackwater-owner}"
KEEPER="${KEEPER:-0xc485487e11aafca2677071c3cd0c83f70872a3d2}"
KEEPER_ACCOUNT="${KEEPER_ACCOUNT:-slackwater-keeper}"

# Our instance, deployed 2026-10-08. Set INSTANCE empty to deploy another.
INSTANCE="${INSTANCE-0x4666E86d6f4989cb80726BD7fB5948891F54d080}"

# AUSD (6 dp) that opens the instance's Perpl account; DeployHedge's argument.
AUSD_SEED_CNS="${AUSD_SEED_CNS:-110000000}"
# MON the owner sends to the instance: both clips.
FUND_MON="${FUND_MON:-3700}"
# MON the owner keeps for gas after funding.
OWNER_GAS_RESERVE_MON="${OWNER_GAS_RESERVE_MON:-100}"
# Lots per clip; one lot is one MON on perp 10.
CLIP_LOTS="${CLIP_LOTS:-1850}"

# Where the record goes, and which round and clip its lines belong to.
ROUNDS_FILE="${ROUNDS_FILE:-data/raw/rounds.jsonl}"
ROUND="${ROUND:-1}"
CLIP="${CLIP:-1}"

# Sign rules for a hedge post. The discount equals the instance's
# hedgeMaxDiscountBps (DeployHedge sets 50); the contract enforces it, this
# only predicts the floor. The age leaves time for inclusion under the
# instance's 60 s limit.
HEDGE_MAX_DISCOUNT_BPS=50
MAX_ORACLE_AGE_TO_SIGN_SEC=40
POST_READ_TRIES=20
POST_READ_INTERVAL_SEC=15
# The first post and 3 requotes.
MAX_POSTS=4

# Exit bounds: the buy-back at most the Perpl ask + 50 bps, the Kuru sale at
# least the Kuru bid - 50 bps; sign only while the Kuru bid is within 30 bps
# under the oracle (decision 32).
EXIT_LIMIT_BPS=50
EXIT_MIN_CASH_BPS=50
EXIT_GUARD_BPS=30
