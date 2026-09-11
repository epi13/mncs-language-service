#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "$script_dir/../.." && pwd)"
data_home="${XDG_DATA_HOME:-$HOME/.local/share}"
syntax_dir="$data_home/org.kde.syntax-highlighting/syntax"

"$script_dir/install.sh" >/dev/null

highlighter=""
for candidate in ksyntaxhighlighter6 kate-syntax-highlighter; do
  if command -v "$candidate" >/dev/null 2>&1; then
    highlighter="$candidate"
    break
  fi
done
if [[ -z "$highlighter" ]]; then
  echo "KSyntaxHighlighting CLI not found; installed XML at $syntax_dir/mncs.xml" >&2
  exit 2
fi

for fixture in \
  "$repo_root/tests/fixtures/records.mncs" \
  "$repo_root/integration/static-syntax/samples/current-profile.mncs"; do
  output="$($highlighter --syntax MNCS --output-format=ansi "$fixture")"
  grep -q 'mncs' <<<"$output"
  grep -q 'fn' <<<"$output"
  printf 'Validated MNCS syntax with %s using %s\n' "$fixture" "$highlighter"
done
