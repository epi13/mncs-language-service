#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "$script_dir/../.." && pwd)"
output_dir="${1:-$repo_root/target/integration}"
stage="$(mktemp -d /tmp/mncs-vscode-extension.XXXXXX)"
trap 'rm -rf -- "$stage"' EXIT

mkdir -p "$stage/syntaxes" "$output_dir"
cp "$script_dir/package.json" "$stage/package.json"
cp "$script_dir/extension.js" "$stage/extension.js"
cp "$script_dir/language-configuration.json" "$stage/language-configuration.json"
cp "$script_dir/.vscodeignore" "$stage/.vscodeignore"
cp "$repo_root/LICENSE" "$stage/LICENSE"
cp "$repo_root/integration/static-syntax/mncs.tmLanguage.json" "$stage/syntaxes/mncs.tmLanguage.json"

npm install --prefix "$stage" --no-package-lock --omit=dev --ignore-scripts >/dev/null
(cd "$stage" && npm exec --yes --package=@vscode/vsce -- vsce package --out "$output_dir/mncs-language-support.vsix") >/dev/null
printf '%s\n' "$output_dir/mncs-language-support.vsix"
