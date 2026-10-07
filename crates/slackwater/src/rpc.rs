use alloy::{
    providers::{DynProvider, Provider, ProviderBuilder},
    rpc::client::RpcClient,
    transports::layers::{RetryBackoffLayer, ThrottleLayer},
};
use anyhow::{Context, ensure};
use perpl_sdk::Chain;

use crate::config::Config;

/// Every request is a single call, never a batch: `ThrottleLayer` paces
/// requests, not the calls inside a batch, and `RetryBackoffLayer` retries the
/// QuickNode "request limit reached" error only when it answers a whole request.
pub async fn connect(config: &Config, chain: &Chain) -> anyhow::Result<DynProvider> {
    let client = RpcClient::builder()
        .layer(ThrottleLayer::new(config.rpc.requests_per_second))
        // A unit cost of 1 makes the retry budget a count of requests a second.
        .layer(
            RetryBackoffLayer::new(
                config.rpc.max_rate_limit_retries,
                config.rpc.initial_backoff_ms,
                u64::from(config.rpc.requests_per_second),
            )
            .with_avg_unit_cost(1),
        )
        .connect(&config.rpc_url)
        .await
        .with_context(|| format!("connecting to {}", config.rpc_url))?;
    let provider = ProviderBuilder::new().connect_client(client).erased();
    let chain_id = provider
        .get_chain_id()
        .await
        .context("reading the chain id")?;
    ensure!(
        chain_id == chain.chain_id(),
        "{} serves chain {chain_id}, expected {}",
        config.rpc_url,
        chain.chain_id()
    );
    Ok(provider)
}
