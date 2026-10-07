#!/usr/bin/env bash
# Compute every statistic from data/raw/ alone, with no network access, into
# data/derived/*.json. Re-running it on the same raw files gives the same output.
#
# Usage: scripts/data/stats.sh
set -euo pipefail

cd "$(dirname "$0")/../.."
# shellcheck source=scripts/data/params.sh
source scripts/data/params.sh

RAW=data/raw
OUT=data/derived
mkdir -p "$OUT"
JQ=(jq -L scripts/data)

labels=$(for m in "${MARKETS[@]}"; do set -- $m; printf '%s %s\n' "$1" "$3"; done |
    jq -R -s -c 'split("\n") | map(select(length > 0) | split(" ") | {key: .[0], value: .[1]}) | from_entries')
fees=$("${JQ[@]}" -s -c 'include "lib";
    map(select(.result) | {key: "\(.fn):\(.perp_id)", value: (.result | hexnum)}) | from_entries' "$RAW/fees.jsonl")

# Funding: one row per interval and perp, from FundingEventCompleted.
# actualRatePct100k is in units of 1e-5 of the funding price per interval, so
# one unit is 0.1 bps; a positive rate pays shorts.
# Days are UTC days of the emitting block; the first and last are partial and
# are left out of the daily statistics.
funding() {
    "${JQ[@]}" -n --slurpfile ev "$RAW/funding-events.jsonl" --slurpfile bl "$RAW/funding-blocks.jsonl" \
        --argjson labels "$labels" '
    include "lib";
    ($bl | map(select(.timestamp)) | map({key: (.block | hexnum | tostring), value: (.timestamp | hexnum)}) | from_entries) as $ts
    | [$ev[] | select(.data) | .data as $d
        | {perp: ($d | word(0) | hexnum), event_block: ($d | word(1) | hexnum), emit_block: (.block | hexnum),
           log_index: (.log_index | hexnum), specified: ($d | word(2) | signed), rate: ($d | word(3) | signed),
           price_pns: ($d | word(4) | hexnum), payment_pns: ($d | word(5) | signed)}
        | .ts = $ts[(.emit_block | tostring)]]
    | group_by(.perp) | map(
        .[0].perp as $p
        | (group_by(.event_block) | map(sort_by(.emit_block, .log_index) | last)) as $iv
        | ($iv | map(.ts | utc_day) | unique) as $days
        | ($iv | group_by(.ts | utc_day) | map({day: (.[0].ts | utc_day), intervals: length,
                bps: (map(.rate * 0.1) | add | r(2))})) as $daily
        | ($daily | map(select(.day != $days[0] and .day != $days[-1]))) as $full
        | ($full | map(.bps)) as $x
        | {perp_id: $p, market: $labels[($p | tostring)],
           from_block: ($iv[0].event_block), to_block: ($iv[-1].event_block),
           from_utc: ($iv[0].ts | todate), to_utc: ($iv[-1].ts | todate),
           intervals: ($iv | length), events: length, overwritten_intervals: (length - ($iv | length)),
           specified_ne_actual: ($iv | map(select(.specified != .rate)) | length),
           total_bps: ($iv | map(.rate * 0.1) | add | r(2)),
           rate_counts: ($iv | group_by(.rate) | map({rate: .[0].rate, n: length})),
           full_days: ($full | length), daily_mean_bps: ($x | mean | r(2)), daily_sd_bps: ($x | sd | r(2)),
           daily_se_bps: (if ($x | length) > 1 then ($x | sd) / (($x | length) | sqrt) | r(2) else null end),
           daily_lag1: ($x | lag1 | r(2)), days_positive: ($x | map(select(. > 0)) | length),
           days_negative: ($x | map(select(. < 0)) | length),
           daily: $daily})'
}

# Price path from the funding price at each interval: the largest rise and the
# largest fall, in percent, over any window of 24 h and 72 h.
price_moves() {
    "${JQ[@]}" -n --slurpfile ev "$RAW/funding-events.jsonl" --slurpfile bl "$RAW/funding-blocks.jsonl" \
        --argjson labels "$labels" '
    include "lib";
    ($bl | map(select(.timestamp)) | map({key: (.block | hexnum | tostring), value: (.timestamp | hexnum)}) | from_entries) as $ts
    | [$ev[] | select(.data) | .data as $d
        | {perp: ($d | word(0) | hexnum), event_block: ($d | word(1) | hexnum), price: ($d | word(4) | hexnum),
           ts: $ts[(.block | hexnum | tostring)]}]
    | group_by(.perp) | map(
        (group_by(.event_block) | map(last) | sort_by(.ts)) as $s
        | def moves($w): [range(0; $s | length) as $i
              | [range($i + 1; $s | length) | select($s[.].ts - $s[$i].ts <= $w) | ($s[.].price / $s[$i].price - 1) * 100]
              | select(length > 0) | {up: max, down: min}];
          (moves(86400)) as $d1 | (moves(259200)) as $d3
        | {perp_id: .[0].perp, market: $labels[(.[0].perp | tostring)],
           max_rise_24h_pct: ($d1 | map(.up) | max | r(2)), max_fall_24h_pct: ($d1 | map(.down) | min | r(2)),
           max_rise_72h_pct: ($d3 | map(.up) | max | r(2)), max_fall_72h_pct: ($d3 | map(.down) | min | r(2)),
           low_vs_last_pct: (($s | map(.price) | min) / $s[-1].price * 100 - 100 | r(2)),
           high_vs_last_pct: (($s | map(.price) | max) / $s[-1].price * 100 - 100 | r(2))})'
}

# Books: per sample block and market, from the same-block readings.
# Entry and exit edges are the taker edges BasisTrader computes at the touch,
# for a clip that fits at the touch, net of the Perpl taker fee: entry buys the
# Kuru ask and shorts the Perpl bid; exit sells the Kuru bid and buys the Perpl
# ask. The Kuru-maker edges assume our resting Kuru order fills at the Kuru
# touch (bid on entry, ask on exit) and the Perpl leg is taker.
books() {
    "${JQ[@]}" -n --slurpfile rows "$RAW/book-samples.jsonl" --argjson labels "$labels" --argjson fees "$fees" '
    include "lib";
    [$rows | group_by(.block)[] | . as $g
        | ($g | map(select(.source == "block"))[0].timestamp | hexnum) as $t
        | $labels | keys[] | tonumber as $p
        | ($g | map(select(.source == "perpl" and .perp_id == $p))[0].result) as $pr
        | ($g | map(select(.source == "kuru" and .perp_id == $p))[0].result) as $kr
        | select($pr != null and $kr != null)
        | ($pr | word(3) | hexnum) as $pd
        | {block: $g[0].block, ts: $t, perp: $p,
           pb: (($pr | word(25) | hexnum) / pow(10; $pd)), pa: (($pr | word(28) | hexnum) / pow(10; $pd)),
           mark: (($pr | word(12) | hexnum) / pow(10; $pd)), oracle: (($pr | word(16) | hexnum) / pow(10; $pd)),
           open_interest_usd: ((($pr | word(18) | hexnum) / pow(10; ($pr | word(4) | hexnum))) * (($pr | word(12) | hexnum) / pow(10; $pd))),
           kb: (($kr | word(0) | hexnum) / 1e18), ka: (($kr | word(1) | hexnum) / 1e18)}
        | .fee = ($fees["getTakerFee:\(.perp)"] / 100)
        | .valid_perpl = (.pb > 0 and .pa > .pb)
        | .valid_kuru = (.kb > 0 and .ka > .kb and .kb < 1e30)
        | if .valid_perpl and .valid_kuru then
            .kuru_spread = ((.ka - .kb) / ((.ka + .kb) / 2) * 1e4)
            | .perpl_spread = ((.pa - .pb) / ((.pa + .pb) / 2) * 1e4)
            | .mid_diff = (((.ka + .kb) / 2) / ((.pa + .pb) / 2) * 1e4 - 1e4)
            | .kuru_vs_oracle = (((.ka + .kb) / 2) / .oracle * 1e4 - 1e4)
            | .taker_entry = ((.pb - .ka) / .ka * 1e4 - .fee)
            | .taker_exit = ((.kb - .pa) / .pa * 1e4 - .fee)
            | .maker_entry = ((.pb - .kb) / .kb * 1e4 - .fee)
            | .maker_exit = ((.ka - .pa) / .pa * 1e4 - .fee)
          else . end]
    | group_by(.perp) | map(
        . as $all | map(select(.valid_perpl and .valid_kuru)) as $v
        | def trips($rows; $e; $x; $ef; $xf):
            ($rows | sort_by(.block) | reduce .[] as $s ({open: null, trips: []};
                if .open == null then (if $s[$e] >= $ef then .open = {edge: $s[$e], ts: $s.ts} else . end)
                elif $s[$x] >= $xf then .trips += [{pnl_bps: (.open.edge + $s[$x]), hold_h: (($s.ts - .open.ts) / 3600)}] | .open = null
                else . end))
            | {round_trips: (.trips | length), mean_pnl_bps: (.trips | map(.pnl_bps) | mean | r(2)), median_pnl_bps: (.trips | map(.pnl_bps) | pct(50) | r(2)),
               mean_hold_h: (.trips | map(.hold_h) | mean | r(2)), open_at_end: (.open != null)};
        {perp_id: .[0].perp, market: $labels[(.[0].perp | tostring)],
         samples: length, valid_samples: ($v | length),
         from_block: (map(.block) | min), to_block: (map(.block) | max),
         from_utc: (map(.ts) | min | todate), to_utc: (map(.ts) | max | todate),
         perpl_one_sided: (map(select(.valid_perpl | not)) | length), kuru_one_sided: (map(select(.valid_kuru | not)) | length),
         taker_fee_bps: .[0].fee,
         kuru_spread_bps: ($v | map(.kuru_spread) | summary), perpl_spread_bps: ($v | map(.perpl_spread) | summary),
         kuru_spread_gt_50_bps: ($v | map(select(.kuru_spread > 50)) | length),
         perpl_spread_gt_50_bps: ($v | map(select(.perpl_spread > 50)) | length),
         kuru_mid_minus_perpl_mid_bps: ($v | map(.mid_diff) | summary),
         kuru_mid_minus_oracle_bps: ($v | map(.kuru_vs_oracle) | summary),
         taker_entry_bps: ($v | map(.taker_entry) | summary), taker_exit_bps: ($v | map(.taker_exit) | summary),
         taker_entry_ge_0: ($v | map(select(.taker_entry >= 0)) | length),
         taker_entry_ge_5: ($v | map(select(.taker_entry >= 5)) | length),
         maker_entry_ge_0: ($v | map(select(.maker_entry >= 0)) | length),
         maker_entry_bps: ($v | map(.maker_entry) | summary),
         taker_exit_ge_0: ($v | map(select(.taker_exit >= 0)) | length),
         taker_exit_ge_minus_5: ($v | map(select(.taker_exit >= -5)) | length),
         # Runs of consecutive samples with an exit edge of at least 0.
         taker_exit_ge_0_runs: ($v | sort_by(.block) | reduce .[] as $s ({runs: [], cur: null};
             if $s.taker_exit >= 0
             then (if .cur == null then .cur = {from_block: $s.block, to_block: $s.block, samples: 1}
                   else .cur.to_block = $s.block | .cur.samples += 1 end)
             else (if .cur == null then . else .runs += [.cur] | .cur = null end) end)
             | (if .cur == null then .runs else .runs + [.cur] end)
             | {runs: length, longest_samples: (map(.samples) | max), samples_in_runs: (map(.samples) | add),
                first_block: (.[0].from_block), last_block: (.[-1].to_block)}),
         # Where the exit edge comes from when it is at least 0: the Kuru bid and
         # the Perpl ask against the Perpl oracle (Chainlink index), and the Kuru spread.
         when_taker_exit_ge_0: ($v | map(select(.taker_exit >= 0))
             | {kuru_bid_vs_oracle_bps: (map((.kb / .oracle - 1) * 1e4) | summary),
                perpl_ask_vs_oracle_bps: (map((.pa / .oracle - 1) * 1e4) | summary),
                kuru_spread_bps: (map(.kuru_spread) | summary)}),
         taker_entry_ge_0_if_taker_fee_zero: ($v | map(select(.taker_entry + .fee >= 0)) | length),
         taker_entry_ge_5_if_taker_fee_zero: ($v | map(select(.taker_entry + .fee >= 5)) | length),
         # A holder who would otherwise sell at the Kuru bid now: hedges at the
         # Perpl bid (taker) at each sample, then exits through the atomic exit
         # at the first later sample whose exit edge clears 0. The result is
         # against selling at the Kuru bid at the arrival sample, before funding.
         holder_arrives_each_sample: ($v | sort_by(.block) as $s
             | (reduce range(($s | length) - 1; -1; -1) as $i ({next: null, out: []};
                 .out[$i] = .next | if $s[$i].taker_exit >= 0 then .next = $i else . end) | .out) as $nx
             | [range(0; $s | length) | . as $i | select($nx[$i] != null)
                | {pnl_bps: ($s[$i].maker_entry + $s[$nx[$i]].taker_exit), wait_h: (($s[$nx[$i]].ts - $s[$i].ts) / 3600)}] as $t
             | {arrivals: ($s | length), exited: ($t | length), never_exited: (($s | length) - ($t | length)),
                pnl_bps: ($t | map(.pnl_bps) | summary), wait_h: ($t | map(.wait_h) | summary)}),
         # The same holder against waiting unhedged and selling at the Kuru bid
         # at the exit sample. The Kuru bid cancels, and so does the price move
         # against the oracle, which is what the hedge removes; what remains is
         # the Perpl premium to the oracle at the hedge, less the Perpl ask
         # premium at the exit, less fees. Taker hedge at the Perpl bid and
         # 3.45 bps; maker hedge assumed filled at the Perpl ask and 0.45 bps.
         # One side of open interest (long equals short), in dollars at the mark.
         open_interest_usd: (map(.open_interest_usd) | summary),
         perpl_bid_vs_oracle_bps: ($v | map((.pb / .oracle - 1) * 1e4) | summary),
         perpl_ask_vs_oracle_bps: ($v | map((.pa / .oracle - 1) * 1e4) | summary),
         holder_vs_waiting_unhedged: ($v | sort_by(.block) as $s
             | (reduce range(($s | length) - 1; -1; -1) as $i ({next: null, out: []};
                 .out[$i] = .next | if $s[$i].taker_exit >= 0 then .next = $i else . end) | .out) as $nx
             | [range(0; $s | length) | . as $i | select($nx[$i] != null) | $s[$nx[$i]] as $x
                | ((($s[$i].pb / $s[$i].oracle) - ($x.pa / $x.oracle)) * 1e4 - $x.fee) as $core
                | {taker_hedge_bps: ($core - $s[$i].fee), maker_hedge_bps: (((($s[$i].pa / $s[$i].oracle) - ($x.pa / $x.oracle)) * 1e4) - $x.fee - 0.45)}] as $t
             | {exited: ($t | length), taker_hedge_bps: ($t | map(.taker_hedge_bps) | summary),
                maker_hedge_bps: ($t | map(.maker_hedge_bps) | summary)}),
         policies: {
           taker_entry_5_exit_minus_5: trips($v; "taker_entry"; "taker_exit"; 5; -5),
           taker_entry_0_exit_minus_5: trips($v; "taker_entry"; "taker_exit"; 0; -5),
           kuru_maker_entry_0_exit_0: trips($v; "maker_entry"; "maker_exit"; 0; 0),
           holder_hedge_entry_0_exit_0: trips($v; "maker_entry"; "taker_exit"; 0; 0),
           holder_hedge_entry_0_exit_0_kuru_spread_le_50: trips($v | map(select(.kuru_spread <= 50)); "maker_entry"; "taker_exit"; 0; 0)}})'
}

# Maker fills on one perp: hourly counts and notional, and fill sizes.
# Times are interpolated linearly between stored block timestamps, every
# BLOCK_TIME_STEP blocks. Price decimals 6 and lot decimals 0 are MON's on Perpl.
fills() {
    "${JQ[@]}" -n --slurpfile f "$RAW/maker-fills.jsonl" --slurpfile bt "$RAW/block-times.jsonl" '
    include "lib";
    ($bt | map(select(.timestamp)) | map({b: .block, t: (.timestamp | hexnum)}) | sort_by(.b)) as $bt
    | ($bt[1].b - $bt[0].b) as $step
    | def time_of($x): ((($x - $bt[0].b) / $step) | floor) as $k
        | $bt[$k].t + ($x - $bt[$k].b) * ($bt[$k + 1].t - $bt[$k].t) / $step;
    [$f[] | select(.words) | (.block | hexnum) as $b
        | {block: $b, ts: time_of($b), account: (.words[1] | hexnum), order: (.words[2] | hexnum),
           price: ((.words[3] | hexnum) / 1e6), lot: (.words[4] | hexnum), fee: ((.words[5] | hexnum) / 1e6)}
        | .usd = (.lot * .price)] as $rows
    | ($rows | map(.ts) | min | . / 3600 | floor) as $h0 | ($rows | map(.ts) | max | . / 3600 | floor) as $h1
    | ($rows | group_by(.ts / 3600 | floor) | map({key: (.[0].ts / 3600 | floor | tostring), value: {n: length, usd: (map(.usd) | add)}}) | from_entries) as $byh
    | [range($h0 + 1; $h1) | $byh[tostring] // {n: 0, usd: 0}] as $hours
    | ($rows | group_by(.account) | map(map(.usd) | add) | sort | reverse) as $acc
    | {fills: ($rows | length), error_windows: ($f | map(select(.error)) | length),
       from_utc: ($rows | map(.ts) | min | todate), to_utc: ($rows | map(.ts) | max | todate),
       full_hours: ($hours | length),
       volume_mon: ($rows | map(.lot) | add), volume_usd: ($rows | map(.usd) | add | r(0)),
       hourly_usd: ($hours | map(.usd) | summary), hourly_fills: ($hours | map(.n) | summary),
       hours_below_1000_usd: ($hours | map(select(.usd < 1000)) | length),
       fill_usd: ($rows | map(.usd) | summary),
       maker_accounts: ($acc | length),
       top3_maker_share_pct: (($acc[0:3] | add) / ($acc | add) * 100 | r(1)),
       window_days: (($rows | map(.ts) | max) - ($rows | map(.ts) | min)) / 86400 | r(2),
       top5_makers_usd: ($rows | group_by(.account) | map({account: .[0].account, usd: (map(.usd) | add | r(0))}) | sort_by(-.usd) | .[0:5]),
       fee_to_notional_ppm: (($rows | map(.fee) | add) / ($rows | map(.usd) | add) * 1e6 | r(1))}'
}

funding >"$OUT/funding.json"
price_moves >"$OUT/price-moves.json"
books >"$OUT/books.json"
if [ -s "$RAW/maker-fills.jsonl" ]; then fills >"$OUT/fills.json"; fi
echo "wrote $(ls "$OUT"/*.json | xargs)"
