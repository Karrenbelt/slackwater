# Hedge, then exit: flows

A holder who owns MON and wants dollars later hedges it now on Perpl with a post-only short, and later sells it through one atomic transaction that closes the short on Perpl and sells the MON on Kuru.
The hedge is not atomic: it rests on Perpl's book until takers fill it.
The exit is atomic: both legs happen, or neither does.
The contract is our fork of Perpl's reference `BasisTrader`, with `hedge`, `cancelHedge` and a guard on the exit added.

Parties: the owner (deploys, funds, can force an exit), the keeper (posts and cancels the hedge, runs the gated exit), `BasisTrader` (holds the MON and the Perpl account), Perpl (the perpetual), Kuru (the spot book).

## States

```mermaid
stateDiagram-v2
    [*] --> Unhedged: owner sends MON to the contract
    Unhedged --> Resting: hedge (post-only short accepted)
    Unhedged --> Unhedged: hedge reverts (price would cross, or short above MON held)
    Resting --> Resting: takers fill part of the short
    Resting --> Hedged: takers fill all of it, then cancelHedge forgets the order
    Resting --> PartlyHedged: cancelHedge removes the unfilled rest
    Resting --> Unhedged: cancelHedge before any fill
    PartlyHedged --> Resting: hedge again for the rest (requote)
    Unhedged --> Resting: hedge again (requote)
    Hedged --> Closed: exit or forceExit (atomic)
    PartlyHedged --> Closed: exit or forceExit with assetIn set to the filled lots
    Resting --> Resting: exit refused while a hedge is recorded
    Hedged --> Hedged: exit reverts (edge below floor, or Kuru pays too little)
    Closed --> [*]: owner sweeps the USDC and any MON left, and withdraws margin
```

While a hedge is recorded, the exit is refused: a resting ask could fill after the MON is sold, which would leave a short with nothing behind it.
The record is cleared only by `cancelHedge`, so it is called before every exit, also after a full fill.
`hedge` refuses a short larger than the MON held, and the exit sells exactly what it closes; the owner must not `sweep` MON while the short is open.

## Hedge: post-only, not atomic

```mermaid
sequenceDiagram
    autonumber
    participant K as Keeper
    participant T as BasisTrader
    participant P as Perpl
    K->>T: hedge(lots, price)
    T->>T: check: no order resting, short plus lots at most the MON held
    T->>P: execOrder(OpenShort, post-only)
    alt price would cross the book
        P-->>T: revert CrossesBook
        T-->>K: revert, nothing recorded
    else order accepted
        P-->>T: order ID
        T->>P: getOrderV2(order ID)
        alt the order is ours, with our size
            T->>T: record the resting order
            T-->>K: HedgePosted
        else anything else
            T-->>K: revert HedgeNotPosted, nothing recorded
        end
    end
```

## While the order rests: fills, funding and requotes

```mermaid
sequenceDiagram
    autonumber
    participant X as Takers
    participant P as Perpl
    participant K as Keeper
    participant T as BasisTrader
    X->>P: buy at our ask
    P-->>T: the short grows by the filled lots (maker fee)
    P-->>T: funding at each funding event, on the filled short
    K->>T: cancelHedge
    T->>T: clear the record
    T->>P: getOrderV2(order ID)
    T->>T: emit HedgeCancelled(unfilled lots, 0 if filled in full)
    alt the order filled in full
        T-->>K: done, no call to Perpl
    else part or none of it filled
        T->>P: execOrder(Cancel, order ID)
        T->>P: getOrderV2(order ID)
        alt the order is gone
            T-->>K: done
        else the order still rests
            T-->>K: revert HedgeNotCancelled: record and event undone, exit stays blocked
        end
    end
    Note over K,T: A requote is cancelHedge, then hedge at the new price, sent after the cancel is mined,<br/>and resent once a block later if Perpl reports too little free margin.
```

## Exit: atomic, all or nothing

```mermaid
sequenceDiagram
    autonumber
    participant K as Keeper or owner
    participant T as BasisTrader
    participant P as Perpl
    participant U as Kuru
    K->>T: exit (keeper, gated) or forceExit (owner, ungated), assetIn set to the hedged MON
    alt a hedge is still recorded
        T-->>K: revert HedgeResting (call cancelHedge first, even after a full fill)
    else no hedge recorded
        T->>P: execOrder(CloseShort, fill-or-kill, at most the limit price)
        T->>U: sell the MON for USDC, at least minCashOut
        T->>T: edge = Kuru proceeds against Perpl buy-back, net of fees
        alt Perpl cannot fill, Kuru pays too little, or (exit only) edge below the floor
            T-->>K: revert: both legs undone, still hedged
        else all checks pass
            T-->>K: Exited(fill): short closed, MON sold, USDC held
        end
    end
```

A refused exit leaves the position as it was: MON held against the short.
The keeper's `exit` waits for an edge at or above the floor; the owner's `forceExit` closes regardless of the edge, within its price bounds, for margin pressure or a deadline.
If the short is smaller than the MON held (a part fill), `assetIn` must be the filled lots in MON; with `assetIn` 0 the exit tries to close as many lots as the whole MON balance, which is more than the short (expected to revert, not tested).

## Compared with the reference contract

The reference `BasisTrader` opens a position by buying spot on Kuru and shorting on Perpl in one atomic, gated transaction, with both legs as takers.
That entry pays Kuru's spread and Perpl's taker fee at once, so its gate rarely opens.
Here the holder already owns the spot, so there is no purchase, and the short is posted as a maker; the reference's atomic, gated exit is kept as it is.
