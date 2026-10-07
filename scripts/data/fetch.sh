#!/usr/bin/env bash
# Fetch raw Monad mainnet readings into data/raw/ and append one manifest row
# per dataset to data/MANIFEST.jsonl. Nothing here computes a statistic;
# stats.sh does that from data/ alone.
#
# Usage: scripts/data/fetch.sh funding|books|fills|fees|purchase <tx hash>|all
#
# Every raw row carries the block it was read at, so it can be re-read on
# chain while the RPC still serves that block: logs about 9,000,000 blocks
# back, state about 1,000,000.
set -euo pipefail

cd "$(dirname "$0")/../.."
# shellcheck source=scripts/data/params.sh
source scripts/data/params.sh

RAW=data/raw
MANIFEST=data/MANIFEST.jsonl
mkdir -p "$RAW"

hex() { printf '0x%x' "$1"; }

# rpc_pass IN OUT CHUNK: post the JSON-RPC requests in IN (one per line) in
# arrays of CHUNK, at most one array a second, and append each response
# object to OUT. A failed HTTP request is retried three times, then aborts.
rpc_pass() {
    local in=$1 out=$2 chunk=$3 part attempt parts
    parts=$(mktemp -d)
    split -l "$chunk" "$in" "$parts/p."
    for part in "$parts"/p.*; do
        for attempt in 1 2 3 4; do
            if jq -s -c . "$part" |
                curl -sf --max-time 120 "$RPC_URL" -H 'content-type: application/json' -d @- |
                jq -c 'if type == "array" then .[] else error("not a batch response") end' >>"$out"; then
                break
            fi
            if [ "$attempt" = 4 ]; then
                echo "rpc_pass: $part failed four times" >&2
                rm -rf "$parts"
                return 1
            fi
            sleep $((attempt * 2))
        done
        sleep 1
    done
    rm -rf "$parts"
}

# rpc_batch IN OUT CHUNK: rpc_pass, then re-post every request whose response
# was the RPC's per-second limit, up to five more passes. Any other error is
# kept in OUT as the response, so it becomes an error row.
rpc_batch() {
    local in=$1 out=$2 chunk=$3 pass todo got limited
    todo=$(mktemp)
    got=$(mktemp)
    limited=$(mktemp)
    cp "$in" "$todo"
    : >"$out"
    for pass in 1 2 3 4 5 6; do
        : >"$got"
        rpc_pass "$todo" "$got" "$chunk"
        jq -c 'select((.error.message // "") | test("request limit") | not)' "$got" >>"$out"
        jq -r 'select((.error.message // "") | test("request limit")) | .id | tojson' "$got" >"$limited"
        [ -s "$limited" ] || break
        echo "rpc_batch: pass $pass, $(wc -l <"$limited") rate-limited, retrying" >&2
        jq -c --slurpfile ids <(jq -s . "$limited" | jq -c '.[] | fromjson') \
            'select(.id as $i | $ids | index($i))' "$in" >"$todo"
        sleep 2
    done
    if [ -s "$limited" ]; then
        jq -c 'select((.error.message // "") | test("request limit"))' "$got" >>"$out"
    fi
    rm -f "$todo" "$got" "$limited"
}

latest_block() {
    curl -sf "$RPC_URL" -H 'content-type: application/json' \
        -d '{"jsonrpc":"2.0","id":1,"method":"eth_blockNumber","params":[]}' | jq -r .result | xargs printf '%d'
}

# manifest DATASET FROM TO STEP ROWS NOTE
manifest() {
    local head dirty
    head=$(git rev-parse HEAD 2>/dev/null || echo unknown)
    if git diff --quiet HEAD -- scripts/data 2>/dev/null && [ -z "$(git ls-files --others --exclude-standard scripts/data)" ]; then
        dirty=false
    else
        dirty=true
    fi
    jq -n -c \
        --arg dataset "$1" --arg rpc "$RPC_URL" --argjson chain "$CHAIN_ID" \
        --argjson from "$2" --argjson to "$3" --argjson step "$4" --argjson rows "$5" --arg note "$6" \
        --arg fetched "$(date -u +%Y-%m-%dT%H:%M:%SZ)" --arg head "$head" --argjson dirty "$dirty" \
        --arg fetch_sha "$(sha256sum scripts/data/fetch.sh | cut -d' ' -f1)" \
        --arg params_sha "$(sha256sum scripts/data/params.sh | cut -d' ' -f1)" \
        '{dataset: $dataset, rpc: $rpc, chain_id: $chain, from_block: $from, to_block: $to, step_blocks: $step,
          rows: $rows, note: $note, fetched_at: $fetched, git_head: $head, scripts_uncommitted: $dirty,
          fetch_sha256: $fetch_sha, params_sha256: $params_sha}' >>"$MANIFEST"
}

perp_ids() { for m in "${MARKETS[@]}"; do set -- $m; echo "$1"; done; }

# FundingEventCompleted for the configured perps over FUNDING_INTERVALS
# intervals, with the timestamp of each emitting block.
fetch_funding() {
    local b last k e req resp out ids
    b=$(latest_block)
    last=$((b / FUNDING_INTERVAL_BLOCKS * FUNDING_INTERVAL_BLOCKS))
    req=$(mktemp)
    resp=$(mktemp)
    for ((k = 0; k < FUNDING_INTERVALS; k++)); do
        e=$((last - k * FUNDING_INTERVAL_BLOCKS))
        for range in "$((e - FUNDING_EMIT_LOOKBACK_BLOCKS)) $((e - LOG_RANGE_BLOCKS))" "$((e - LOG_RANGE_BLOCKS + 1)) $e"; do
            set -- $range
            jq -n -c --arg a "$PERPL_EXCHANGE" --arg f "$(hex "$1")" --arg t "$(hex "$2")" \
                --arg topic "$TOPIC_FUNDING_EVENT_COMPLETED" --argjson id "$k" \
                '{jsonrpc: "2.0", id: $id, method: "eth_getLogs",
                  params: [{address: $a, fromBlock: $f, toBlock: $t, topics: [$topic]}]}' >>"$req"
        done
    done
    rpc_batch "$req" "$resp" "$BATCH_LOGS"

    # The perp id is the first data word; keep our perps' logs with the fields
    # that identify and carry the event. A failed request is an error row.
    local words
    words=$(perp_ids | while read -r p; do printf '%064x\n' "$p"; done | jq -R -s -c 'split("\n") | map(select(length > 0))')
    out=$RAW/funding-events.jsonl
    jq -c --argjson words "$words" '
        if .error then {interval_index: .id, error: .error}
        else .result[] | select(.data[2:66] as $w | $words | index($w))
            | {block: .blockNumber, tx: .transactionHash, log_index: .logIndex, data: .data}
        end' "$resp" >"$out"

    # Timestamps of the emitting blocks.
    : >"$req"
    jq -r 'select(.block) | .block' "$out" | sort -u | while read -r blk; do
        jq -n -c --arg b "$blk" '{jsonrpc: "2.0", id: ($b | ltrimstr("0x")), method: "eth_getBlockByNumber", params: [$b, false]}' >>"$req"
    done
    rpc_batch "$req" "$resp" "$BATCH_CALLS"
    jq -c 'if .error then {error: .error} else {block: .result.number, timestamp: .result.timestamp} end' "$resp" >"$RAW/funding-blocks.jsonl"

    manifest funding "$((last - (FUNDING_INTERVALS - 1) * FUNDING_INTERVAL_BLOCKS - FUNDING_EMIT_LOOKBACK_BLOCKS))" "$last" \
        "$FUNDING_INTERVAL_BLOCKS" "$(wc -l <"$out")" \
        "FundingEventCompleted logs for perps $(perp_ids | xargs), $FUNDING_INTERVALS intervals; raw/funding-events.jsonl and raw/funding-blocks.jsonl"
    rm -f "$req" "$resp"
}

# One sample every BOOK_STEP_BLOCKS: for each market, Perpl getPerpetualInfoV2
# and Kuru bestBidAsk, plus the block timestamp, all at the same block.
# Oldest first, because the oldest samples leave the RPC's retention first.
fetch_books() {
    local b top j s req resp out call
    b=$(latest_block)
    top=$((b / BOOK_STEP_BLOCKS * BOOK_STEP_BLOCKS))
    req=$(mktemp)
    resp=$(mktemp)
    # One template per call, then one request per sample block.
    local templates
    templates=$(
        {
            jq -n -c '{source: "block"}'
            for m in "${MARKETS[@]}"; do
                set -- $m
                jq -n -c --arg p "$1" --arg to "$PERPL_EXCHANGE" --arg d "$(cast calldata 'getPerpetualInfoV2(uint256)' "$1")" \
                    '{source: "perpl", perp: $p, to: $to, data: $d}'
                jq -n -c --arg p "$1" --arg to "$2" --arg d "$(cast calldata 'bestBidAsk()')" \
                    '{source: "kuru", perp: $p, to: $to, data: $d}'
            done
        } | jq -s -c .
    )
    for ((j = BOOK_SAMPLES - 1; j >= 0; j--)); do
        echo $((top - j * BOOK_STEP_BLOCKS))
    done | jq -c --argjson t "$templates" '. as $s | $t[]
        | if .source == "block"
          then {jsonrpc: "2.0", id: "\($s):block", method: "eth_getBlockByNumber", params: [null, false]}
          else {jsonrpc: "2.0", id: "\($s):\(.source):\(.perp)", method: "eth_call", params: [{to: .to, data: .data}, null]}
          end' >"$req.tmpl"
    # jq has no hex formatting, so the block tag is filled in by printf.
    while read -r line; do
        s=${line#*\"id\":\"}
        s=${s%%:*}
        printf '%s\n' "${line//null/\"$(hex "$s")\"}"
    done <"$req.tmpl" >"$req"
    rm -f "$req.tmpl"
    rpc_batch "$req" "$resp" "$BATCH_CALLS"
    out=$RAW/book-samples.jsonl
    # id is "<block>:<source>[:<perp id>]"; a block row keeps only its timestamp.
    jq -c '(.id | split(":")) as $k
        | {block: ($k[0] | tonumber), source: $k[1], perp_id: ($k[2] // null | if . then tonumber else null end)}
          + (if .error then {error: .error}
             elif $k[1] == "block" then {timestamp: .result.timestamp}
             else {result: .result} end)' "$resp" |
        jq -s -c 'sort_by(.block, .source, .perp_id) | .[]' >"$out"
    manifest books "$((top - (BOOK_SAMPLES - 1) * BOOK_STEP_BLOCKS))" "$top" "$BOOK_STEP_BLOCKS" "$(wc -l <"$out")" \
        "Same-block Perpl getPerpetualInfoV2 (raw ABI result) and Kuru bestBidAsk per market, with block timestamps; samples older than the RPC's state retention are error rows"
    rm -f "$req" "$resp"
}

# Maker fills on FILL_PERP_ID over FILL_WINDOW_BLOCKS, as the event's data
# words, and block timestamps every BLOCK_TIME_STEP blocks across the window.
fetch_fills() {
    local b top from a req resp out word
    b=$(latest_block)
    top=$((b / LOG_RANGE_BLOCKS * LOG_RANGE_BLOCKS))
    from=$((top - FILL_WINDOW_BLOCKS + 1))
    req=$(mktemp)
    resp=$(mktemp)
    for ((a = from; a <= top; a += LOG_RANGE_BLOCKS)); do
        jq -n -c --arg x "$PERPL_EXCHANGE" --arg f "$(hex "$a")" --arg t "$(hex $((a + LOG_RANGE_BLOCKS - 1)))" \
            --arg t1 "$TOPIC_MAKER_ORDER_FILLED_V2" --arg t2 "$TOPIC_MAKER_ORDER_FILLED" --argjson id "$a" \
            '{jsonrpc: "2.0", id: $id, method: "eth_getLogs",
              params: [{address: $x, fromBlock: $f, toBlock: $t, topics: [[$t1, $t2]]}]}' >>"$req"
    done
    rpc_batch "$req" "$resp" "$BATCH_LOGS"
    word=$(printf '%064x' "$FILL_PERP_ID")
    out=$RAW/maker-fills.jsonl
    # Data words, in order: perpId, accountId, orderId, pricePNS, lotLNS,
    # feeCNS, lockedBalanceCNS, amountCNS (signed), balanceCNS[, builderId,
    # builderFeeCNS]. Leading zeros are trimmed, which loses nothing; a signed
    # word must be left-padded to 64 digits again before it is read as signed.
    jq -c --arg w "$word" '
        if .error then {from_block: .id, error: .error}
        else .result[] | select(.data[2:66] == $w)
            | {block: .blockNumber, tx: .transactionHash, log_index: .logIndex, topic: .topics[0],
               words: [range(0; ((.data | length) - 2) / 64) as $i | .data[2 + 64 * $i: 66 + 64 * $i] | sub("^0+(?=.)"; "")]}
        end' "$resp" >"$out"

    : >"$req"
    for ((a = from - 1; a <= top + BLOCK_TIME_STEP; a += BLOCK_TIME_STEP)); do
        [ "$a" -le "$b" ] || break
        jq -n -c --arg b "$(hex "$a")" --argjson id "$a" \
            '{jsonrpc: "2.0", id: $id, method: "eth_getBlockByNumber", params: [$b, false]}' >>"$req"
    done
    rpc_batch "$req" "$resp" "$BATCH_CALLS"
    jq -c 'if .error then {block: .id, error: .error} else {block: .id, timestamp: .result.timestamp} end' "$resp" >"$RAW/block-times.jsonl"

    manifest fills "$from" "$top" "$LOG_RANGE_BLOCKS" "$(wc -l <"$out")" \
        "MakerOrderFilled and MakerOrderFilledV2 logs on perp $FILL_PERP_ID, as data words; raw/block-times.jsonl every $BLOCK_TIME_STEP blocks"
    rm -f "$req" "$resp"
}

# Taker and maker fee per market at the latest block.
fetch_fees() {
    local b req resp
    b=$(latest_block)
    req=$(mktemp)
    resp=$(mktemp)
    for m in "${MARKETS[@]}"; do
        set -- $m
        for f in getTakerFee getMakerFee; do
            jq -n -c --arg b "$(hex "$b")" --arg id "$f:$1" --arg to "$PERPL_EXCHANGE" --arg d "$(cast calldata "$f(uint256)" "$1")" \
                '{jsonrpc: "2.0", id: $id, method: "eth_call", params: [{to: $to, data: $d}, $b]}' >>"$req"
        done
    done
    rpc_batch "$req" "$resp" "$BATCH_CALLS"
    jq -c --argjson b "$b" '(.id | split(":")) as $k
        | {block: $b, fn: $k[0], perp_id: ($k[1] | tonumber)} + (if .error then {error: .error} else {result: .result} end)' \
        "$resp" >"$RAW/fees.jsonl"
    manifest fees "$b" "$b" 0 "$(wc -l <"$RAW/fees.jsonl")" "Perpl getTakerFee and getMakerFee per market, ppm"
    rm -f "$req" "$resp"
}

# The receipt of one conversion, as its block and ERC-20 Transfer logs.
fetch_purchase() {
    local tx=$1 out=$RAW/purchase-$1.json
    curl -sf "$RPC_URL" -H 'content-type: application/json' \
        -d "$(jq -n -c --arg h "$tx" '{jsonrpc: "2.0", id: 1, method: "eth_getTransactionReceipt", params: [$h]}')" |
        jq --arg t "$TOPIC_ERC20_TRANSFER" '.result | {tx: .transactionHash, block: .blockNumber, from, to, status,
            transfers: [.logs[] | select(.topics[0] == $t) | {token: .address, from: .topics[1], to: .topics[2], amount: .data}]}' >"$out"
    local blk
    blk=$(jq -r .block "$out" | xargs printf '%d')
    manifest purchase "$blk" "$blk" 0 1 "Receipt Transfer logs of $tx"
}

case "${1:-}" in
    funding) fetch_funding ;;
    books) fetch_books ;;
    fills) fetch_fills ;;
    fees) fetch_fees ;;
    purchase) fetch_purchase "${2:?transaction hash}" ;;
    all) fetch_books && fetch_fees && fetch_funding && fetch_fills ;;
    *)
        echo "usage: $0 funding|books|fills|fees|purchase <tx hash>|all" >&2
        exit 2
        ;;
esac
