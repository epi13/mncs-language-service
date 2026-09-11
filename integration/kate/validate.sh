#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
command -v jq >/dev/null 2>&1 || {
  echo "jq is required to validate the Kate project example" >&2
  exit 2
}

jq -e '.files[0].filters | index("*.mncs")' \
  "$script_dir/mncs.kateproject.example" >/dev/null
jq -e '.lspclient.servers.mncs.command == ["mncs-lsp"]' \
  "$script_dir/mncs.kateproject.example" >/dev/null
jq -e '.lspclient.servers.mncs.path | index("%{ENV:HOME}/.local/bin")' \
  "$script_dir/mncs.kateproject.example" >/dev/null
jq -e '.lspclient.servers.mncs.highlightingModeRegex == "^MNCS$"' \
  "$script_dir/mncs.kateproject.example" >/dev/null

printf 'Validated Kate project configuration: %s\n' \
  "$script_dir/mncs.kateproject.example"
