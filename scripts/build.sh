#!/usr/bin/env bash
# Build both contracts to wasm and copy them into res/ (prebuilt copies are committed).
# Needs Rust >= 1.95 with the wasm32-unknown-unknown target, and cargo-near
# (https://github.com/near/cargo-near) for the club.
set -euo pipefail
cd "$(dirname "$0")/.."
TARGET="${CARGO_TARGET_DIR:-$PWD/target}"
# wallet: no SDK, plain cargo build (cargo-near only supports SDK contracts)
RUSTFLAGS="-C link-arg=-s" cargo build -p satchel-wallet --release --target wasm32-unknown-unknown
cp "$TARGET/wasm32-unknown-unknown/release/satchel_wallet.wasm" res/
# club: near-sdk contract, built and optimized by cargo-near
(cd contracts/club && cargo near build non-reproducible-wasm)
cp "$TARGET/near/satchel_club/satchel_club.wasm" res/
cp "$TARGET/near/satchel_club/satchel_club_abi.json" res/
ls -l res/
sha256sum res/*.wasm
