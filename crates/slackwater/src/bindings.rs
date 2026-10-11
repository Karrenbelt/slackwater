//! Kuru bindings, generated from upstream's ABI files, and `BasisTrader`'s,
//! generated from the Foundry artifact of the pinned fork, so they cannot
//! drift from the deployed source; `just build-sc` writes the artifact before
//! the crate builds. Perpl's come from `perpl_sdk::abi`.

// `sol!` generates functions as wide as the ABI they mirror.
#![allow(clippy::too_many_arguments)]

alloy::sol!(
    #[sol(rpc)]
    KuruOrderBook,
    "../../upstream/8ball030/basis_trade/abi/kuru/OrderBook.json"
);

alloy::sol!(
    #[sol(rpc)]
    #[derive(Debug)]
    BasisTrader,
    "../../upstream/8ball030/basis_trade/smart_contracts/out/BasisTrader.sol/BasisTrader.json"
);
