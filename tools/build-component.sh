#!/usr/bin/env bash
# Build the WIT component and browser-neutral generated instantiation wrapper
# from this exact checkout.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
toolchain=${ESBT_RUST_TOOLCHAIN:-1.95.0}

if [[ -n "${CARGO:-}" ]]; then
  cargo_command=("$CARGO")
elif command -v rustup >/dev/null 2>&1 \
  && rustup which cargo --toolchain "$toolchain" >/dev/null 2>&1 \
  && rustc_bin=$(rustup which rustc --toolchain "$toolchain" 2>/dev/null); then
  cargo_command=(env "RUSTC=$rustc_bin" rustup run "$toolchain" cargo)
else
  cargo_command=(cargo)
fi

"${cargo_command[@]}" build \
  --locked \
  --release \
  --target wasm32-unknown-unknown \
  --manifest-path "$root/Cargo.toml"

core="$root/target/wasm32-unknown-unknown/release/esbt.wasm"
component="$root/target/wasm32-unknown-unknown/release/esbt.component.wasm"
generated="$root/web/generated"
node "$root/tools/build-component.mjs" \
  "$core" \
  "$component" \
  "$generated" \
  "$root/target/wasm32-unknown-unknown/release/esbt.component.wit"
node "$root/tools/verify-component.mjs"
