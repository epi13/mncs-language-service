#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "$script_dir/../.." && pwd)"
output_dir="${MNCS_VSCODE_OUTPUT_DIR:-$repo_root/target/integration}"
vsix="$($script_dir/package.sh "$output_dir")"

code_bin="${CODE_BIN:-code}"
"$code_bin" --install-extension "$vsix" --force
printf 'Installed MNCS VS Code extension from %s\n' "$vsix"
