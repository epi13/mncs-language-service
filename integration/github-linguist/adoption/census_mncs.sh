#!/usr/bin/env bash
# Census of public .mncs usage on GitHub — rerunnable readiness measurement.
#
# Three measurement channels, in decreasing reliability:
#
#   1. Family census (default): counts `.mncs` files on the default branch of
#      each known MNCS-family repository via the GitHub trees API. Fully
#      scriptable and exact for those repositories.
#
#   2. Legacy REST code search (`search/code`): printed for completeness but
#      KNOWN-UNRELIABLE for rare extensions — its index omits most small
#      repositories (calibration: it also returns 0 for `path:*.gleam`, an
#      accepted Linguist language with thousands of files). Do not treat a
#      0 here as evidence of absence.
#
#   3. Logged-in code search: the authoritative channel Linguist maintainers
#      use ("assessed by manually and randomly clicking through the
#      results"). It cannot be queried without an interactive session, so
#      this script prints the exact URL to check by hand.
#
# Requires: gh (authenticated), jq, curl.
set -euo pipefail

REPOS=(
  epi13/mncs-language
  epi13/mncs-language-service
  epi13/RAVEL
  epi13/mncs-lineage
  epi13/mncs-validator-rs
  epi13/machine-native-complexity-standard
  epi13/Machine-Native-Experimental-Learning
)

echo "# .mncs adoption census — $(date -u +%Y-%m-%d)"
echo
echo "## 1. Known-family census (GitHub trees API, default branch)"
echo
printf '| Repository | .mncs files |\n| --- | --- |\n'
total=0
for repo in "${REPOS[@]}"; do
  count=$(gh api "repos/$repo/git/trees/HEAD?recursive=1" \
    --jq '[.tree[] | select(.type=="blob" and (.path | endswith(".mncs")))] | length' 2>/dev/null || echo "unavailable")
  if [[ "$count" =~ ^[0-9]+$ ]]; then
    total=$((total + count))
  fi
  printf '| %s | %s |\n' "$repo" "$count"
done
printf '| **Total (known family)** | **%d** |\n' "$total"

owners=$(printf '%s\n' "${REPOS[@]}" | cut -d/ -f1 | sort -u | wc -l)
echo
echo "Distinct owners in known-family set: $owners"
echo "(Linguist requires reasonable distribution across unique :user/:repo.)"

echo
echo "## 2. Legacy REST code search (known-unreliable index)"
for q in 'path:*.mncs' 'path:*.gleam'; do
  n=$(curl -s -H "Accept: application/vnd.github+json" \
    "https://api.github.com/search/code?q=$(python3 -c "import urllib.parse,sys;print(urllib.parse.quote(sys.argv[1]))" "$q")" \
    | python3 -c "import json,sys;d=json.load(sys.stdin);print(d.get('total_count', d.get('message','?')))" 2>/dev/null || echo "?")
  echo "  $q => $n"
done
echo "  (calibration: *.gleam returning 0 proves this index cannot measure"
echo "   rare extensions; ignore these numbers)"

echo
echo "## 3. Authoritative logged-in code search (manual step)"
echo "  https://github.com/search?type=code&q=NOT+is%3Afork+path%3A*.mncs"
echo "  Linguist's bar: >=2000 files/extension/year excluding forks for"
echo "  multi-file extensions (.mncs qualifies) AND plausible distribution."
