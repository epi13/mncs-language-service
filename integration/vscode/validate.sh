#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
node --check "$script_dir/extension.js"
jq empty "$script_dir/package.json"
jq empty "$script_dir/language-configuration.json"
grep -q 'source.mncs' "$script_dir/../static-syntax/mncs.tmLanguage.json"
printf '%s\n' 'Validated VS Code adapter source and manifests'
