//! Kuru bindings, generated from upstream's ABI files. Perpl's come from
//! `perpl_sdk::abi`.

// `sol!` generates functions as wide as the ABI they mirror.
#![allow(clippy::too_many_arguments)]

alloy::sol!(
    KuruOrderBook,
    "../../upstream/8ball030/basis_trade/abi/kuru/OrderBook.json"
);
