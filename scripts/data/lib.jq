# Helpers for stats.sh. jq numbers are IEEE doubles: exact to 2^53, and
# relative error about 1e-16 above that, which is far below a basis point.

def nibble: if . >= 97 then . - 87 elif . >= 65 then . - 55 else . - 48 end;

# Unsigned hex string (with or without 0x) to a number.
def hexnum: (if startswith("0x") then .[2:] else . end)
    | explode | reduce .[] as $c (0; . * 16 + ($c | nibble));

# Two's-complement 256-bit word to a signed number; a negative value is
# computed from its complement so that small magnitudes stay exact.
def signed: (if startswith("0x") then .[2:] else . end) as $h
    | if ($h[0:1] | test("[89a-fA-F]"))
      then -((($h | explode | reduce .[] as $c (0; . * 16 + (15 - ($c | nibble))))) + 1)
      else $h | hexnum end;

# Word i (0-based) of an ABI-encoded hex string.
def word($i): .[2 + 64 * $i: 66 + 64 * $i];

def mean: if length == 0 then null else add / length end;
def sd: if length < 2 then null else (mean) as $m | (map((. - $m) * (. - $m)) | add) / (length - 1) | sqrt end;
# Nearest-rank percentile, p in [0, 100].
def pct($p): if length == 0 then null else sort | .[((($p / 100) * (length - 1)) | round)] end;
def lag1: if length < 3 then null else
    (mean) as $m | (map(. - $m)) as $d
    | ([range(0; length - 1) | $d[.] * $d[. + 1]] | add) / ($d | map(. * .) | add) end;
def r($n): if . == null then null else (. * pow(10; $n) | round) / pow(10; $n) end;
def summary: {n: length, mean: (mean | r(2)), sd: (sd | r(2)), p05: (pct(5) | r(2)), p50: (pct(50) | r(2)),
    p95: (pct(95) | r(2)), min: (min | r(2)), max: (max | r(2))};
def utc_day: strftime("%Y-%m-%d");
