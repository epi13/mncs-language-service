#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
data_home="${XDG_DATA_HOME:-$HOME/.local/share}"
syntax_dir="$data_home/org.kde.syntax-highlighting/syntax"
mkdir -p "$syntax_dir"
install -m 0644 "$script_dir/mncs.xml" "$syntax_dir/mncs.xml"
printf 'Installed MNCS syntax definition: %s\n' "$syntax_dir/mncs.xml"
