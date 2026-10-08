#!/usr/bin/env bash
# Operates a BasisTrader hedge round on Monad mainnet with Foundry's cast and
# forge: reads, the signed steps, and an append-only record of both in
# data/raw/rounds.jsonl (schema in data/README.md).
#
# Usage: scripts/round/round.sh <command> [args]   (`help` lists the commands)
# Parameters: scripts/round/params.sh, each overridable from the environment.
#
# A signing command reads, checks, simulates, prints what it will sign and
# asks y/N. It signs with a Foundry keystore, never a raw key. A failed read is
# retried and then stops the command; it is never taken as a value.
set -euo pipefail

cd "$(dirname "$0")/../.."
# shellcheck source=scripts/round/params.sh
source scripts/round/params.sh

SC=upstream/8ball030/basis_trade/smart_contracts

PI='getPerpetualInfoV2(uint256)((string,string,uint256,uint256,bytes32,uint256,uint256,uint256,uint256,uint256,uint256,uint256,uint256,uint256,uint256,uint256,uint256,uint256,uint256,uint256,int16,uint256,uint8,uint256,uint256,uint256,uint256,uint256,uint256,bool,uint256))'
OV='getOrderV2(uint256,uint256)((uint32,uint8,uint24,uint40,uint16,uint32,uint16,uint16,uint16,uint16,uint16,uint8,uint16))'
POS='position(uint256)((uint256,uint256,uint256,uint8,uint256,uint256,uint256,uint256,int256,int256,int256,uint256),uint256,bool)'
ACCT='perplAccount()((uint256,uint256,uint256,uint8,address,(uint256,uint256,uint256,uint256)))'
MARKET='markets(uint256)(bool,address,address,uint8,uint8,uint8,uint16,uint16,uint32,int64,int64,uint32,uint32)'
FSUM='getFundingSumAtBlock(uint256,uint256)(int48,uint256)'
MOF='MakerOrderFilledV2(uint256,uint256,uint256,uint256,uint256,uint256,uint256,int256,uint256,uint256,uint256)'
HEDGE_T='(uint256,uint256,uint256,uint256,uint256)'
EXIT_T='(uint256,uint256,uint32,address[],bool[],bool[],uint256,uint256,uint256,uint256)'
FILL_T='(uint256,uint256,uint256,uint256,uint256,int256)'
# keccak256("HedgeCancelled(uint256,uint256,uint256,uint256)")
HEDGE_CANCELLED=0x1d549c5a0c56333f34f6a7f3b9a3f417c5a4a8727b4be1da44f4824ae4cdd0b1
# keccak256("PositionOpenedV2(...)") and keccak256("PositionIncreasedV2(...)")
POSITION_OPENED=0x04cc3d2fc73a9dca30eba1d05eca80b1b1216350243580027046f434fed4db18
POSITION_INCREASED=0x99a74f70c224396b9ba5fcd5a6e5f480db23e7a25a2b16a8c133ec2efb3e646c

die() {
    echo "$*" >&2
    exit 1
}

need_instance() {
    [[ -n $INSTANCE ]] || die "INSTANCE is empty: deploy first, or set it"
}

# ------------------------------------------------------------------ chain

# A read through cast, retried on failure. Never used for `cast send`.
rd() {
    local i
    for i in 1 2 3; do
        if cast "$@" --rpc-url "$RPC"; then
            return 0
        fi
        echo "read failed (attempt $i of 3): cast $1" >&2
        sleep 2
    done
    return 1
}

# A simulated call. Prints the result and returns 0, or prints the revert and
# returns 2; a read failure is retried, and returns 1 after three.
sim() {
    local i out
    for i in 1 2 3; do
        if out=$(cast call "$@" --rpc-url "$RPC" 2>&1); then
            echo "$out"
            return 0
        fi
        if [[ $out == *"execution reverted"* ]]; then
            echo "$out"
            return 2
        fi
        echo "simulation read failed (attempt $i of 3): $out" >&2
        sleep 2
    done
    return 1
}

# Signs with keystore $1 and waits for the receipt. Prints the hash of any
# mined transaction and returns 0 if it succeeded, 3 if it reverted; returns 1
# with no hash if cast failed before a receipt.
send() {
    local account=$1 out h st
    shift
    if ! out=$(cast send "$@" --account "$account" --rpc-url "$RPC" --json); then
        echo "cast send failed before a receipt; nothing is known to be sent: check \`cast nonce\` of the signer" >&2
        return 1
    fi
    h=$(jq -r .transactionHash <<<"$out")
    st=$(jq -r .status <<<"$out")
    echo "tx $h, status $st" >&2
    echo "$h"
    [[ $st == 0x1 ]] || return 3
}

# Signs as `send` does, for step $1 with record fields $2. A mined transaction
# that reverted paid gas, so it is recorded before the command stops.
signed() {
    local step=$1 extra=$2 h st
    shift 2
    h=$(send "$@") && st=0 || st=$?
    ((st != 1)) || exit 1
    if ((st == 3)); then
        rec "$step" "$h" "$(jq -c '. + {reverted: true}' <<<"$extra")"
        die "$step: transaction $h reverted; it is recorded"
    fi
    echo "$h"
}

confirm() {
    local a
    read -r -p "$1 [y/N] " a </dev/tty
    [[ $a == y ]] || die "not signed"
}

errname() {
    local pair
    for pair in 0xb853e584:AmountExceedsAvailableBalance 0x44d9c22e:OracleStale 0x8782b317:OracleUnusable \
        0x267b50e1:HedgeBelowOracle 0xbeb39429:CrossesBook 0x3ec7413b:HedgeResting 0x1464be18:HedgeNotCancelled \
        0x6e053bcd:NoRestingHedge 0xa16ae1a1:Unhedgeable 0xf03a741e:EdgeTooThin 0x7c795110:SpotFillTooSmall \
        0xb0b199e7:PegOutOfBand 0x604559a5:CloseOrderExceedsPosition; do
        if [[ $1 == *"${pair%%:*}"* ]]; then
            echo "${pair#*:}"
            return
        fi
    done
    echo unknown
}

blk() {
    if [[ -n ${1:-} ]]; then echo "$1"; else rd block-number; fi
}

# ------------------------------------------------------------------ reads
# Each takes an optional block and prints one JSON line that names it.

# Perpl perp: `floor` is the lowest price `hedge` accepts against this oracle.
perp() {
    local n t out
    n=$(blk "${1:-}")
    t=$(rd block "$n" -f timestamp)
    out=$(rd call "$PERPL" "$PI" "$PERP_ID" --block "$n" --json)
    jq -c --argjson n "$n" --argjson now "$t" --argjson d "$HEDGE_MAX_DISCOUNT_BPS" '.[0] | {
        block: $n, mark: (.[11]|tonumber), oracle: (.[15]|tonumber),
        oracleAgeSec: ($now - (.[16]|tonumber)), bid: (.[24]|tonumber), ask: (.[27]|tonumber),
        ignOracle: .[29], fundingRatePct100k: (.[20]|tonumber),
        floor: ((.[15]|tonumber) * (10000 - $d) / 10000 | ceil)}' <<<"$out"
}

# Kuru touch: USDC (6 dp) per MON (18 dp), scaled by 1e18; strings, as they
# exceed 2^53.
kuru() {
    local n out
    n=$(blk "${1:-}")
    out=$(rd call "$BOOK" 'bestBidAsk()(uint256,uint256)' --block "$n" --json)
    jq -c --argjson n "$n" '{block: $n, bid: .[0], ask: .[1]}' <<<"$out"
}

acct() {
    local n out
    need_instance
    n=$(blk "${1:-}")
    out=$(rd call "$INSTANCE" "$ACCT" --block "$n" --json)
    jq -c --argjson n "$n" '.[0] | {block: $n, accountId: (.[0]|tonumber),
        balanceCNS: (.[1]|tonumber), lockedBalanceCNS: (.[2]|tonumber)}' <<<"$out"
}

pos() {
    local n out
    need_instance
    n=$(blk "${1:-}")
    out=$(rd call "$INSTANCE" "$POS" "$PERP_ID" --block "$n" --json)
    jq -c --argjson n "$n" '{block: $n, positionType: (.[0][3]|tonumber), depositCNS: (.[0][4]|tonumber),
        pricePNS: (.[0][5]|tonumber), lotLNS: (.[0][6]|tonumber), premiumPnlCNS: (.[0][10]|tonumber),
        markPNS: (.[1]|tonumber), markValid: .[2]}' <<<"$out"
}

order() {
    local id=$1 n out
    n=$(blk "${2:-}")
    out=$(rd call "$PERPL" "$OV" "$PERP_ID" "$id" --block "$n" --json)
    jq -c --argjson n "$n" '.[0] | {block: $n, accountId: (.[0]|tonumber), orderType: (.[1]|tonumber),
        priceONS: (.[2]|tonumber), lotLNS: (.[3]|tonumber), leverageHdths: (.[6]|tonumber)}' <<<"$out"
}

fundsum() {
    local n out
    n=$(blk "${1:-}")
    out=$(rd call "$PERPL" "$FSUM" "$PERP_ID" "$n" --block "$n" --json)
    jq -c --argjson n "$n" '{block: $n, fundingSum: (.[0]|tonumber), setAtBlock: (.[1]|tonumber)}' <<<"$out"
}

resting() {
    need_instance
    rd call "$INSTANCE" 'restingHedge(uint256)(uint256)' "$PERP_ID"
}

account_id() {
    need_instance
    rd call "$INSTANCE" 'perplAccountId()(uint256)'
}

# ------------------------------------------------------------------ record

# One line for a mined transaction. $3 is a JSON object merged last, so it
# can override `clip`.
rec() {
    local step=$1 tx=$2 extra=${3:-'{}'} g r
    g=$(rd tx "$tx" gas)
    r=$(rd receipt "$tx" --json)
    jq -c -L scripts/data --arg step "$step" --argjson gasLimit "$g" --argjson extra "$extra" \
        --argjson round "$ROUND" --argjson clip "$CLIP" 'include "lib";
        {round: $round, clip: $clip, step: $step, tx: .transactionHash, block: (.blockNumber|hexnum),
         status: (.status|hexnum), gasUsed: (.gasUsed|hexnum), gasLimit: $gasLimit,
         effectiveGasPriceWei: (.effectiveGasPrice|hexnum),
         logs: [.logs[] | {address, topic0: .topics[0], data}]} + $extra' <<<"$r" >>"$ROUNDS_FILE"
}

# One line for a read.
note() {
    jq -c --arg step "$1" --argjson round "$ROUND" --argjson clip "$CLIP" \
        '{round: $round, clip: $clip, step: $step} + .' <<<"$2" >>"$ROUNDS_FILE"
}

# 1 + the number of this round's and clip's lines whose step starts with $1.
next_index() {
    local c
    c=$(jq -c --arg p "$1" --argjson round "$ROUND" --argjson clip "$CLIP" \
        'select(.round == $round and .clip == $clip and (.step | startswith($p)))' "$ROUNDS_FILE" 2>/dev/null | wc -l)
    echo $((c + 1))
}

last_hedge() {
    jq -sc --argjson round "$ROUND" --argjson clip "$CLIP" \
        '[.[] | select(.round == $round and .clip == $clip and (.step | startswith("hedge-")))] | last' "$ROUNDS_FILE"
}

# ------------------------------------------------------------------ fills

# The first block in (lo, hi] whose short exceeds `base` lots.
# Precondition: lots(lo) <= base < lots(hi).
first_grown() {
    local lo=$1 hi=$2 base=$3 mid l
    while ((hi - lo > 1)); do
        mid=$(((lo + hi) / 2))
        l=$(pos "$mid" | jq .lotLNS)
        if ((l > base)); then hi=$mid; else lo=$mid; fi
    done
    echo "$hi"
}

# Our maker fills in the 100 blocks ending at $1; the public RPC refuses
# eth_getLogs over more than 100 blocks.
fills_at() {
    local b=$1 acc perp out
    acc=$(cast to-uint256 "$(account_id)")
    perp=$(cast to-uint256 "$PERP_ID")
    out=$(rd logs --from-block $((b - 99)) --to-block "$b" --address "$PERPL" "$MOF" --json)
    jq -c -L scripts/data --arg acc "$acc" --arg perp "$perp" 'include "lib"; .[]
        | select((.data|word(0)) == $perp[2:] and (.data|word(1)) == $acc[2:])
        | {tx: .transactionHash, block: (.blockNumber|hexnum), orderId: (.data|word(2)|hexnum),
           pricePNS: (.data|word(3)|hexnum), lotLNS: (.data|word(4)|hexnum),
           feeCNS: (.data|word(5)|hexnum), builderFeeCNS: (.data|word(10)|hexnum)}' <<<"$out"
}

# The Perpl logs of a transaction whose second word is our account ID: the
# maker fill and the position's open or increase.
tx_logs() {
    local acc out
    acc=$(cast to-uint256 "$(account_id)")
    out=$(rd receipt "$1" --json)
    jq -c -L scripts/data --arg acc "$acc" --arg perpl "${PERPL,,}" 'include "lib";
        [.logs[] | select((.address|ascii_downcase) == $perpl and (.data|word(1)) == $acc[2:])
         | {address, topic0: .topics[0], data}]' <<<"$out"
}

# ------------------------------------------------------------------ commands

cmd_state() {
    local n
    n=$(rd block-number)
    echo "instance $INSTANCE, restingHedge $(resting)"
    perp "$n"
    kuru "$n"
    acct "$n"
    pos "$n"
}

cmd_watch() {
    local id=$1 n
    while true; do
        if ! n=$(cast block-number --rpc-url "$RPC"); then
            echo "$(date +%T) block read failed, retrying"
            sleep 5
            continue
        fi
        echo "$(date +%T) $(order "$id" "$n" || echo 'order read failed') $(pos "$n" || echo 'position read failed')"
        sleep 30
    done
}

cmd_deploy_sim() {
    (cd "$SC" && forge script script/DeployHedge.s.sol --sig 'run(address,uint256)' "$KEEPER" "$AUSD_SEED_CNS" \
        --rpc-url "$RPC" --sender "$OWNER")
}

# Read-backs of a deployed instance against DeployHedge's settings. Prints
# each, and the deploy-readback JSON on the last line if all hold.
readback() {
    local n fails=0 got
    n=$(rd block-number)
    check() {
        [[ -n $2 ]] || die "read-back of $1 could not be read"
        if [[ ${2,,} == "${3,,}" ]]; then echo "ok   $1 = $2" >&2; else echo "FAIL $1 = $2, expected $3" >&2; fails=$((fails + 1)); fi
    }
    check owner "$(rd call "$INSTANCE" 'owner()(address)' --block "$n")" "$OWNER"
    check keeper "$(rd call "$INSTANCE" 'keepers(address)(bool)' "$KEEPER" --block "$n")" true
    check market "$(rd call "$INSTANCE" "$MARKET" "$PERP_ID" --block "$n" --json | jq -c 'map(tostring) | join(",")')" \
        '"true,0x0000000000000000000000000000000000000000,0x754704bc059f8c67012fed69bc8a327a5aafb603,18,6,0,100,1000,2000,9223372036854775807,0,1000000,1000000"'
    check hedgeMaxDiscountBps "$(rd call "$INSTANCE" 'hedgeMaxDiscountBps(uint256)(uint16)' "$PERP_ID" --block "$n")" "$HEDGE_MAX_DISCOUNT_BPS"
    check hedgeMaxOracleAgeSec "$(rd call "$INSTANCE" 'hedgeMaxOracleAgeSec(uint256)(uint32)' "$PERP_ID" --block "$n")" 60
    got=$(acct "$n")
    if ((fails > 0)); then
        echo "$fails read-backs failed" >&2
        return 1
    fi
    jq -c --arg i "$INSTANCE" --arg o "$OWNER" --arg k "$KEEPER" --argjson d "$HEDGE_MAX_DISCOUNT_BPS" \
        '{clip: null, block: .block, instance: $i, owner: $o, keeper: $k, leverageHdths: 100,
          minEntryEdgePpm: "9223372036854775807", minExitEdgePpm: 0, pegBandPpm: [1000000, 1000000],
          hedgeMaxDiscountBps: $d, hedgeMaxOracleAgeSec: 60, acct: .}' <<<"$got"
}

cmd_deploy() {
    local chain f h fn line
    if [[ -n $INSTANCE && $(rd code "$INSTANCE") != 0x ]]; then
        die "INSTANCE=$INSTANCE already has code; set INSTANCE= (empty) to deploy another"
    fi
    echo "deploy: owner $OWNER deploys DeployHedge with keeper $KEEPER and $AUSD_SEED_CNS CNS of AUSD (7 transactions)"
    echo "run \`$0 deploy-sim\` first and check its owner, perplAccountId and transaction count"
    confirm "Broadcast as $OWNER_ACCOUNT?"
    (cd "$SC" && forge script script/DeployHedge.s.sol --sig 'run(address,uint256)' "$KEEPER" "$AUSD_SEED_CNS" \
        --rpc-url "$RPC" --account "$OWNER_ACCOUNT" --sender "$OWNER" --broadcast)
    chain=$(rd chain-id)
    f="$SC/broadcast/DeployHedge.s.sol/$chain/run-latest.json"
    INSTANCE=$(jq -r '.transactions[] | select(.transactionType == "CREATE") | .contractAddress' "$f")
    echo "instance $INSTANCE"
    while IFS=$'\t' read -r fn h; do
        rec "deploy-$fn" "$h" '{"clip":null}'
    done < <(jq -r '.transactions[] | "\((.function // "create") | split("(")[0])\t\(.hash)"' "$f")
    if line=$(readback); then
        note deploy-readback "$line"
    else
        die "the deploy is recorded; its read-backs failed and are not"
    fi
    echo "set INSTANCE=$INSTANCE for the next commands"
}

cmd_fund() {
    local inst own h after
    need_instance
    inst=$(rd balance "$INSTANCE")
    [[ $inst == 0 ]] || die "the instance already holds $inst wei; not sending"
    own=$(rd balance "$OWNER" --ether)
    jq -en --argjson o "$own" --argjson m "$FUND_MON" --argjson r "$OWNER_GAS_RESERVE_MON" '$o >= $m + $r' >/dev/null ||
        die "the owner holds $own MON, under $FUND_MON + $OWNER_GAS_RESERVE_MON"
    echo "fund: owner $OWNER sends $FUND_MON MON to $INSTANCE (the owner holds $own MON)"
    confirm "Sign as $OWNER_ACCOUNT?"
    h=$(signed fund-mon '{"clip":null}' "$OWNER_ACCOUNT" "$INSTANCE" --value "${FUND_MON}ether")
    after=$(rd balance "$INSTANCE")
    echo "the instance holds $after wei"
    rec fund-mon "$h" "$(jq -nc --arg w "$after" '{clip: null, instanceWei: $w}')"
}

# The keeper posts $1 lots (default a clip) one tick under the Perpl ask,
# once the sign rules hold: oracle usable and young enough, a gap of more
# than one tick, and ask - 1 at or above the bound's floor.
cmd_post() {
    local lots=${1:-$CLIP_LOTS} k n p P="" i age bid ask floor a kv base args out rc h id o mine a2
    need_instance
    k=$(next_index hedge-)
    ((k <= MAX_POSTS)) || die "$MAX_POSTS posts already (the first and the requotes); stopping"
    [[ $(resting) == 0 ]] || die "a hedge is recorded; run cancel first"
    for ((i = 1; i <= POST_READ_TRIES; i++)); do
        n=$(rd block-number)
        p=$(perp "$n")
        age=$(jq .oracleAgeSec <<<"$p")
        bid=$(jq .bid <<<"$p")
        ask=$(jq .ask <<<"$p")
        floor=$(jq .floor <<<"$p")
        if [[ $(jq .ignOracle <<<"$p") == false ]] && ((age <= MAX_ORACLE_AGE_TO_SIGN_SEC && ask - bid > 1 && ask - 1 >= floor)); then
            P=$((ask - 1))
            break
        fi
        echo "rules not met, reading again in ${POST_READ_INTERVAL_SEC}s: $p" >&2
        sleep "$POST_READ_INTERVAL_SEC"
    done
    [[ -n $P ]] || die "the sign rules did not hold in $POST_READ_TRIES reads"
    a=$(acct "$n")
    kv=$(kuru "$n")
    base=$(pos "$n" | jq .lotLNS)
    args="($PERP_ID,$lots,$P,0,$n)"
    out=$(sim "$INSTANCE" "hedge($HEDGE_T)(uint256)" "$args" --from "$KEEPER") && rc=0 || rc=$?
    ((rc == 0)) || die "simulation: $(errname "$out") $out"
    echo "hedge-$k: keeper posts $lots lots at $P PNS, requestId $n"
    echo "  perp $p"
    echo "  kuru $kv"
    echo "  simulated order id $out"
    confirm "Sign as $KEEPER_ACCOUNT?"
    h=$(signed "hedge-$k" "$(jq -nc --argjson lots "$lots" --argjson P "$P" --argjson n "$n" \
        '{lots: $lots, pricePNS: $P, requestId: $n}')" "$KEEPER_ACCOUNT" "$INSTANCE" "hedge($HEDGE_T)" "$args")
    id=$(resting)
    o=$(order "$id")
    mine=$(account_id)
    a2=$(acct)
    echo "order: $o"
    echo "  expected accountId $mine, orderType 1, priceONS $P, lotLNS $lots"
    echo "acct before: $a"
    echo "acct after:  $a2"
    note acct-before-post "$(jq -c --argjson k "$k" '. + {post: $k}' <<<"$a")"
    rec "hedge-$k" "$h" "$(jq -nc --argjson p "$p" --argjson kv "$kv" --argjson lots "$lots" --argjson P "$P" \
        --argjson n "$n" --argjson id "$id" --argjson base "$base" \
        '{perp: $p, kuru: $kv, lots: $lots, pricePNS: $P, requestId: $n, orderId: $id, positionLotsBefore: $base}')"
    note acct-after-post "$(jq -c --argjson k "$k" '. + {post: $k}' <<<"$a2")"
}

# The keeper clears the recorded hedge: it cancels the order if it is still
# ours, and only clears the record if it is not.
cmd_cancel() {
    local label=${1:-} id n o q mine live=false out rc h q2 o2 r u last base posted now
    id=$(resting)
    [[ $id != 0 ]] || die "no hedge is recorded"
    n=$(rd block-number)
    o=$(order "$id" "$n")
    q=$(pos "$n")
    mine=$(account_id)
    [[ $(jq .accountId <<<"$o") == "$mine" ]] && live=true
    out=$(sim "$INSTANCE" 'cancelHedge(uint256,uint256)(uint256)' "$PERP_ID" "$n" --from "$KEEPER") && rc=0 || rc=$?
    ((rc == 0)) || die "simulation: $(errname "$out") $out"
    [[ -n $label ]] || label=cancel-$(next_index cancel-)
    echo "$label: keeper clears order $id (still ours: $live; unfilled lots returned: $out)"
    echo "  position $q"
    confirm "Sign as $KEEPER_ACCOUNT?"
    h=$(signed "$label" "$(jq -nc --argjson id "$id" '{orderId: $id}')" \
        "$KEEPER_ACCOUNT" "$INSTANCE" 'cancelHedge(uint256,uint256)' "$PERP_ID" "$n")
    q2=$(pos)
    o2=$(order "$id")
    r=$(resting)
    u=$(rd receipt "$h" --json | jq -c -L scripts/data --arg t "$HEDGE_CANCELLED" --arg i "${INSTANCE,,}" \
        'include "lib"; [.logs[] | select((.address|ascii_downcase) == $i and .topics[0] == $t) | (.data|word(1)|hexnum)] | first')
    echo "after: restingHedge $r (expected 0); order $o2; position $q2; HedgeCancelled unfilled $u"
    rec "$label" "$h" "$(jq -nc --argjson id "$id" --argjson live "$live" --argjson u "$u" --argjson q "$q2" \
        '{orderId: $id, oursBeforeCancel: $live, unfilledLotLNS: $u, positionAfter: $q}')"
    # An order that left the book without filling: Perpl's open-interest cap,
    # or an admin cancel.
    last=$(last_hedge)
    base=$(jq .positionLotsBefore <<<"$last")
    posted=$(jq .lots <<<"$last")
    now=$(jq .lotLNS <<<"$q2")
    if [[ $live == false ]] && ((now < base + posted)); then
        note order-gone "$(jq -nc --argjson id "$id" --argjson n "$n" --argjson now "$now" '{orderId: $id, block: $n, positionLots: $now}')"
        echo "recorded order-gone: the order left the book with the position at $now lots"
    fi
}

# The remainder is measured from the position before the clip's first post,
# because earlier clips stay in the same position.
cmd_requote() {
    local first base now lots
    first=$(jq -sc --argjson round "$ROUND" --argjson clip "$CLIP" \
        '[.[] | select(.round == $round and .clip == $clip and (.step | startswith("hedge-")))] | first' "$ROUNDS_FILE")
    [[ $first != null ]] || die "no hedge post in $ROUNDS_FILE for round $ROUND clip $CLIP"
    base=$(jq .positionLotsBefore <<<"$first")
    cmd_cancel
    now=$(pos | jq .lotLNS)
    lots=$((base + CLIP_LOTS - now))
    if ((lots <= 0)); then
        echo "the position is the whole clip; nothing to requote"
        return
    fi
    cmd_post "$lots"
}

# Finds and records our maker fills since the last post: bisects the position
# over past state, then reads the 100 blocks ending at each fill.
cmd_findfill() {
    local last lo base hi now b fills f tx logs p k
    last=$(last_hedge)
    [[ $last != null ]] || die "no hedge post in $ROUNDS_FILE for round $ROUND clip $CLIP"
    lo=$(jq .block <<<"$last")
    base=$(jq .positionLotsBefore <<<"$last")
    hi=$(rd block-number)
    now=$(pos "$hi" | jq .lotLNS)
    ((now > base)) || die "no fill since block $lo; the position is $now lots"
    while ((base < now)); do
        b=$lo
        if (($(pos "$lo" | jq .lotLNS) <= base)); then
            b=$(first_grown "$lo" "$hi" "$base")
        fi
        fills=$(fills_at "$b")
        [[ -n $fills ]] || die "the position grew at block $b, but no fill of ours is in the 100 blocks ending there"
        while read -r f; do
            tx=$(jq -r .tx <<<"$f")
            if jq -se --arg tx "$tx" 'any(.[]; .step == "fill" and .tx == $tx)' "$ROUNDS_FILE" >/dev/null; then
                echo "already recorded: $tx"
                continue
            fi
            logs=$(tx_logs "$tx")
            p=$(perp "$b")
            k=$(kuru "$b")
            note fill "$(jq -c --argjson p "$p" --argjson k "$k" --argjson logs "$logs" \
                '. + {perp: $p, kuru: $k, logs: $logs}' <<<"$f")"
            echo "recorded fill: $f"
        done <<<"$fills"
        base=$(pos "$b" | jq .lotLNS)
        lo=$b
    done
}

# After the fill: clear the record, record the fills and the account.
cmd_afterfill() {
    cmd_cancel cancel-final
    cmd_findfill
    note acct-after-fill "$(acct)"
    echo "position: $(pos)"
}

# Read only, then recorded: the forced and the keeper's exit of $1 lots
# (default the clip) at the plan's bounds, and whether the guard holds.
cmd_exitsim() {
    local lots=${1:-$CLIP_LOTS} n p k ask oracle bid gap lim min x fe ke rfe rke line
    need_instance
    n=$(rd block-number)
    p=$(perp "$n")
    k=$(kuru "$n")
    ask=$(jq .ask <<<"$p")
    oracle=$(jq .oracle <<<"$p")
    bid=$(jq -r .bid <<<"$k")
    # Both in USDC with 6 dp: the oracle in PNS, the Kuru bid / 1e12.
    gap=$(jq -n "($oracle - $bid / 1e12) / $oracle * 10000")
    lim=$((ask * (10000 + EXIT_LIMIT_BPS) / 10000))
    min=$(jq -n "$lots * $bid / 1e12 * (10000 - $EXIT_MIN_CASH_BPS) / 10000 | ceil")
    x="($PERP_ID,${lots}000000000000000000,1000000,[$BOOK],[false],[true],$min,$lim,100,$n)"
    fe=$(sim "$INSTANCE" "forceExit($EXIT_T)($FILL_T)" "$x" --from "$OWNER" --block "$n") && rfe=0 || rfe=$?
    ((rfe != 1)) || die "the forceExit simulation could not be read; nothing recorded"
    ke=$(sim "$INSTANCE" "exit($EXIT_T)($FILL_T)" "$x" --from "$KEEPER" --block "$n") && rke=0 || rke=$?
    ((rke != 1)) || die "the exit simulation could not be read; nothing recorded"
    line=$(jq -nc --argjson n "$n" --argjson p "$p" --argjson k "$k" --argjson gap "$gap" --argjson g "$EXIT_GUARD_BPS" \
        --argjson lots "$lots" --argjson lim "$lim" --argjson min "$min" \
        --arg fe "$fe" --arg ke "$ke" --arg nfe "$(errname "$fe")" --arg nke "$(errname "$ke")" \
        --argjson rfe "$rfe" --argjson rke "$rke" \
        '{block: $n, perp: $p, kuru: $k, gapBps: $gap, guard: ($gap <= $g), lots: $lots, lim: $lim, min: $min,
          forceExit: (if $rfe == 0 then $fe else "reverted " + $nfe end),
          exit: (if $rke == 0 then $ke else "reverted " + $nke end), raw: {forceExit: $fe, exit: $ke}}')
    jq . <<<"$line"
    note exit-sim "$line"
}

# Equity E = balanceCNS + position depositCNS (the order lock is inside
# balanceCNS), from the first acct-before-post to the last acct-after-fill,
# against the fees in the recorded fills.
cmd_reconcile() {
    local before after b0 b1 e0 e1 fees split
    before=$(jq -sc --argjson round "$ROUND" --argjson clip "$CLIP" \
        '[.[] | select(.round == $round and .clip == $clip and .step == "acct-before-post")] | first' "$ROUNDS_FILE")
    after=$(jq -sc --argjson round "$ROUND" --argjson clip "$CLIP" \
        '[.[] | select(.round == $round and .clip == $clip and .step == "acct-after-fill")] | last' "$ROUNDS_FILE")
    [[ $before != null && $after != null ]] || die "need acct-before-post and acct-after-fill lines"
    b0=$(jq .block <<<"$before")
    b1=$(jq .block <<<"$after")
    e0=$(($(jq .balanceCNS <<<"$before") + $(pos "$b0" | jq .depositCNS)))
    e1=$(($(jq .balanceCNS <<<"$after") + $(pos "$b1" | jq .depositCNS)))
    fees=$(jq -s --argjson round "$ROUND" --argjson clip "$CLIP" \
        '[.[] | select(.round == $round and .clip == $clip and .step == "fill") | .feeCNS + .builderFeeCNS] | add // 0' "$ROUNDS_FILE")
    split=$(jq -s -L scripts/data --argjson round "$ROUND" --argjson clip "$CLIP" \
        --arg o "$POSITION_OPENED" --arg i "$POSITION_INCREASED" 'include "lib";
        [.[] | select(.round == $round and .clip == $clip and .step == "fill") | .logs[]
         | select(.topic0 == $o or .topic0 == $i) | .data
         | if (length - 2) / 64 == 11 then (word(8)|hexnum) + (word(9)|hexnum) else (word(12)|hexnum) + (word(13)|hexnum) end]
        | add // 0' "$ROUNDS_FILE")
    echo "E at $b0: $e0; E at $b1: $e1; change $((e1 - e0))"
    echo "fees (feeCNS + builderFeeCNS): $fees; gap $((e1 - e0 + fees))"
    echo "insFeeCNS + protFeeCNS in the position logs: $split (the split of feeCNS, not added to it)"
    echo "funding sum: $(fundsum "$b0") -> $(fundsum "$b1")"
}

usage() {
    cat <<EOF
Usage: scripts/round/round.sh <command> [args]

Reads (no signing):
  state                 restingHedge, Perpl, Kuru, account and position at one block
  perp|kuru|acct|pos [block]
  order <id> [block]
  watch <orderId>       every 30 s, the order and the position; Ctrl-C to stop
  findfill              find and record our maker fills since the last post
  exitsim [lots]        simulate forceExit and exit at the exit bounds; records exit-sim
  reconcile             account equity against the recorded fill fees
  readback              the instance's settings against DeployHedge's

Signed, each after a simulation and a y/N:
  deploy-sim            forge script DeployHedge without broadcast (no key)
  deploy                broadcast DeployHedge as the owner; records it
  fund                  the owner sends FUND_MON MON to the instance
  post [lots]           the keeper posts a hedge one tick under the Perpl ask
  cancel [label]        the keeper clears the recorded hedge (records order-gone if it left unfilled)
  requote               cancel, then post the unfilled rest of the clip
  afterfill             cancel-final, findfill, and the account read

Recording by hand:
  rec <step> <tx> [json]
  note <step> <json>

Instance $INSTANCE; round $ROUND, clip $CLIP; record $ROUNDS_FILE.
EOF
}

cmd=${1:-help}
shift || true
case $cmd in
state) cmd_state ;;
perp) perp "${1:-}" ;;
kuru) kuru "${1:-}" ;;
acct) acct "${1:-}" ;;
pos) pos "${1:-}" ;;
order) order "$1" "${2:-}" ;;
watch) cmd_watch "$1" ;;
findfill) cmd_findfill ;;
exitsim) cmd_exitsim "${1:-}" ;;
reconcile) cmd_reconcile ;;
readback) need_instance && readback ;;
deploy-sim) cmd_deploy_sim ;;
deploy) cmd_deploy ;;
fund) cmd_fund ;;
post) cmd_post "${1:-}" ;;
cancel) cmd_cancel "${1:-}" ;;
requote) cmd_requote ;;
afterfill) cmd_afterfill ;;
rec) rec "$@" ;;
note) note "$@" ;;
help | -h | --help) usage ;;
*)
    usage >&2
    exit 2
    ;;
esac
