#!/usr/bin/env bash

set -euo pipefail

DEMO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PYTHON_PROJECT="${DEMO_DIR}/projects/python"
RUST_PROJECT="${DEMO_DIR}/projects/rust"
TYPESCRIPT_PROJECT="${DEMO_DIR}/projects/typescript"

cargo fmt --manifest-path "${RUST_PROJECT}/Cargo.toml"
uv run --project "${PYTHON_PROJECT}" black "${PYTHON_PROJECT}/src"
npm run --prefix "${TYPESCRIPT_PROJECT}" format
