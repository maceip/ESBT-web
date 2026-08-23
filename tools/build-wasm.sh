#!/usr/bin/env bash
# Build the engine-owned browser artifact from this exact checkout. Marks has
# a stricter release wrapper that additionally records source/profile hashes.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
toolchain=${ESBT_RUST_TOOLCHAIN:-1.95.0}
if [[ -n "${CARGO:-}" ]]; then
  cargo_command=("$CARGO")
elif command -v rustup >/dev/null 2>&1 \
  && rustup which cargo --toolchain "$toolchain" >/dev/null 2>&1 \
  && rustc_bin=$(rustup which rustc --toolchain "$toolchain" 2>/dev/null); then
  # `rustup run` uses an already-installed toolchain and never installs one.
  # Bind rustc as well: Homebrew may precede the rustup shims on PATH.
  cargo_command=(env "RUSTC=$rustc_bin" rustup run "$toolchain" cargo)
else
  cargo_command=(cargo)
fi

if ! "${cargo_command[@]}" build \
  --locked \
  --release \
  --target wasm32-unknown-unknown \
  --manifest-path "$root/Cargo.toml"; then
  echo "ESBT Wasm build failed; install wasm32-unknown-unknown for Rust $toolchain or set CARGO" >&2
  exit 1
fi

built="$root/target/wasm32-unknown-unknown/release/esbt.wasm"
artifact="$root/web/esbt.wasm"
mkdir -p "$root/web"
cp "$built" "$artifact"
node "$root/tools/wasm-abi.mjs" --verify-wasm "$artifact"
