#!/usr/bin/env bash
# Compare a pinned ABI with the code deployed behind an ERC-1967 proxy.
#
# Usage: scripts/abi-drift.sh <proxy address> <abi json> <event name>...
#
# Function selectors are compared as sets, in both directions. Each named
# event's topic must occur somewhere in the runtime code: that is a presence
# test on bytes, weaker than the selector comparison, because a topic hash can
# sit in the code without the event being emitted. Both tests concern the
# implementation now, not the one that emitted any stored log.
set -euo pipefail

proxy=$1
abi=$2
shift 2
rpc=${MONAD_RPC_URL:?MONAD_RPC_URL is not set}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# keccak256("eip1967.proxy.implementation") - 1
slot=0x360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc
word=$(cast storage "$proxy" "$slot" --rpc-url "$rpc")
impl=0x${word:26}
cast code "$impl" --rpc-url "$rpc" >"$work/code"
[ "$(wc -c <"$work/code")" -gt 4 ] || { echo "no code at implementation $impl" >&2; exit 1; }

# Canonical signatures need tuple types expanded into their components.
canonical='def t: if (.type | startswith("tuple"))
        then "(" + ([.components[] | t] | join(",")) + ")" + (.type | ltrimstr("tuple"))
        else .type end;
    "\(.name)(\([.inputs[] | t] | join(",")))"'
abi_items() { jq -r --arg kind "$1" "(.abi // .)[] | select(.type == \$kind) | $canonical" "$abi"; }

# The runtime code is longer than one shell argument may be, so it goes in on stdin.
cast selectors <"$work/code" | cut -f1 | sort -u >"$work/deployed"
abi_items function | while read -r sig; do echo "$(cast sig "$sig") $sig"; done | sort >"$work/pinned"
cut -d' ' -f1 "$work/pinned" | sort -u >"$work/pinned.sel"

status=0
while read -r sel; do
    echo "in the ABI, not deployed: $(grep -F "$sel " "$work/pinned")" >&2
    status=1
done < <(comm -23 "$work/pinned.sel" "$work/deployed")
while read -r sel; do
    echo "deployed, not in the ABI: $sel" >&2
    status=1
done < <(comm -13 "$work/pinned.sel" "$work/deployed")

for name in "$@"; do
    sig=$(abi_items event | grep -m1 "^$name(" || true)
    if [ -z "$sig" ]; then
        echo "event not in the ABI: $name" >&2
        status=1
        continue
    fi
    topic=$(cast keccak "$sig")
    if ! grep -qF "${topic#0x}" "$work/code"; then
        echo "topic not in the code: $sig $topic" >&2
        status=1
    fi
done

echo "implementation $impl: $(wc -l <"$work/deployed") deployed selectors, $(wc -l <"$work/pinned.sel") in the ABI, $# event topics checked"
exit "$status"
