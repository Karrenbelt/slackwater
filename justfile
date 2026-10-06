set shell := ["bash", "-euo", "pipefail", "-c"]

export MONAD_RPC_URL := env("MONAD_RPC_URL", "https://rpc.monad.xyz")
export MONAD_TESTNET_RPC_URL := env("MONAD_TESTNET_RPC_URL", "https://testnet-rpc.monad.xyz")

# Upstream is read-only to us: nothing here rewrites it.
sc := "upstream/8ball030/basis_trade/smart_contracts"
ex := "upstream/8ball030/basis_trade/executor"
rs := "crates/slackwater"
forge_std := "foundry-rs/forge-std@v1.17.0"
oz := "OpenZeppelin/openzeppelin-contracts@v5.1.0"

_default:
    @just --list --unsorted

deps:
    cd {{ sc }} && forge install {{ forge_std }} --no-git
    cd {{ sc }} && forge install {{ oz }} --no-git

build: build-sc build-ex build-rs

build-sc:
    @test -d {{ sc }}/lib/forge-std || just deps
    cd {{ sc }} && forge build --no-lint

build-ex: build-sc
    cd {{ ex }} && cargo build

build-rs:
    cd {{ rs }} && cargo build

fmt:
    cd {{ rs }} && cargo fmt

fmt-check:
    cd {{ sc }} && forge fmt --check
    cd {{ ex }} && cargo fmt --check
    cd {{ rs }} && cargo fmt --check

lint: build-sc
    cd {{ sc }} && forge lint src/ test/ script/
    cd {{ ex }} && cargo clippy --all-targets -- -D warnings
    cd {{ rs }} && cargo clippy --all-targets -- -D warnings
    cd {{ rs }} && cargo machete

test: build-sc abi-drift
    cd {{ sc }} && forge test -vv --network monad --no-match-contract VenuePerplTest
    cd {{ ex }} && cargo test
    cd {{ rs }} && cargo test

test-offline: build-sc
    cd {{ ex }} && cargo test
    cd {{ rs }} && cargo test

validate: fmt-check lint test

precommit: fmt-check

prepush: lint

hooks:
    @git config core.hooksPath scripts/hooks

fork:
    anvil --fork-url "$MONAD_RPC_URL"

abi-drift abi="upstream/dex-sdk/crates/sdk/abi/dex/Exchange.json":
    scripts/abi-drift.sh 0x34B6552d57a35a1D042CcAe1951BD1C370112a6F {{ abi }} \
        FundingEventCompleted MakerOrderFilledV2 OrderRequestV2 TakerOrderFilledV2 OrderBatchCompleted

demo:
    ./scripts/demo.sh

clean:
    cd {{ sc }} && forge clean
    cd {{ ex }} && cargo clean
    cd {{ rs }} && cargo clean
