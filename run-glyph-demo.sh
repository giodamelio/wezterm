#!/usr/bin/env bash
#
# Build wezterm-gui and a glyph generator, then launch a standalone wezterm
# (--always-new-process) with the Glyph Protocol enabled and stream a set of
# registrations plus labelled lines for visual verification.
#
# The payloads come from wezterm-glyph-protocol/examples/gen_test_glyphs.rs,
# built with the same wire format the crate decodes, with a test asserting
# every one parses and rasterizes.

set -euo pipefail

cd "$(dirname "$0")"
REPO="$PWD"

cargo build -p wezterm-gui
cargo build -p wezterm-glyph-protocol --example gen_test_glyphs

TARGET_DIR="${CARGO_TARGET_DIR:-$REPO/target}"
GEN="$TARGET_DIR/debug/examples/gen_test_glyphs"
if [[ ! -x "$GEN" ]]; then
    echo "error: generator not found at $GEN" >&2
    exit 1
fi

# --config enables the protocol for this instance only; your config is untouched.
cargo run -p wezterm-gui -- \
    --config enable_glyph_protocol=true \
    start --always-new-process -- \
    bash -c "
        '$GEN'
        printf '\nPress Enter in this window to close it.\n'
        read -r _
    "
