#!/usr/bin/env bash

set -euo pipefail

DEMO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

generate() {
  local language="$1"
  local source="$2"
  local project="$3"
  local output_dir="${DEMO_DIR}/generated/${language}"

  mkdir -p "${output_dir}"
  arborium --html --lang "${language}" "${source}" > "${output_dir}/arborium.html"
  lighter --format html --project "${project}" "${source}" > "${output_dir}/lighter.html"
}

generate "rust" \
  "${DEMO_DIR}/projects/rust/src/main.rs" \
  "${DEMO_DIR}/projects/rust"
generate "python" \
  "${DEMO_DIR}/projects/python/src/demo.py" \
  "${DEMO_DIR}/projects/python"

TYPESCRIPT_PROJECT="${DEMO_DIR}/projects/typescript"
TYPESCRIPT_SERVER="${TYPESCRIPT_PROJECT}/node_modules/.bin/typescript-language-server"

if [[ ! -x "${TYPESCRIPT_SERVER}" ]]; then
  echo "Install the TypeScript fixture dependencies before generating HTML." >&2
  exit 1
fi

PATH="${TYPESCRIPT_PROJECT}/node_modules/.bin:${PATH}" \
  generate "typescript" \
    "${TYPESCRIPT_PROJECT}/src/demo.ts" \
    "${TYPESCRIPT_PROJECT}"
