#!/bin/bash
set -euo pipefail

echo "Building frontend..."
cd "$(dirname "$0")/../frontend" && npm run build && cd ..

echo "Building agentiso..."
cargo build --release

echo "Done! Binary at target/release/agentiso"
