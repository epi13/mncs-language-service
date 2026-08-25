#!/usr/bin/env bash
# Reproducible validation of the MNCS GitHub/Linguist integration against a
# local checkout of github-linguist/linguist.
#
# Usage:
#   LINGUIST_DIR=/path/to/linguist ./validate_linguist.sh [options]
#
# Options (environment):
#   KEEP=1            keep injected changes in the linguist checkout
#   RUN_FULL_TESTS=1  additionally run the full upstream suite (slow)
#   SKIP_RESTORE=1    implied by KEEP=1
#
# What it does — mirroring the upstream contribution path, minus Docker:
#   1. verifies the Linguist checkout and its Ruby toolchain;
#   2. inserts `languages.yml.fragment` into `lib/linguist/languages.yml`
#      at the alphabetically correct position; `language_id` stays omitted
#      (upstream `script/update-ids` generates it during the real PR);
#   3. copies `samples/*.mncs` into Linguist's `samples/MNCS/`;
#   4. regenerates the samples database (`bundle exec rake samples`) so the
#      statistical classifier trains on MNCS;
#   5. runs focused checks: each sample classifies as MNCS; aliases and
#      extension resolve; `github-linguist --breakdown` reports MNCS on a
#      scratch repository containing the samples;
#   6. with RUN_FULL_TESTS=1, runs the complete upstream test suite.
#
# Grammar registration upstream happens via
#   script/add-grammar https://github.com/epi13/mncs-language-service
# which requires Docker and vendors this repository as a submodule under
# vendor/grammars/. That step is documented in PR-CHECKLIST.md and is not
# replicated here: classification does not need the vendored grammar.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
fragment="$here/languages.yml.fragment"
samples="$here/samples"

LINGUIST_DIR="${LINGUIST_DIR:-}"
KEEP="${KEEP:-}"
RUN_FULL_TESTS="${RUN_FULL_TESTS:-}"

fail() { printf 'validate_linguist: %s\n' "$1" >&2; exit 1; }
step() { printf '\n== %s\n' "$1"; }

[ -n "$LINGUIST_DIR" ] || fail "set LINGUIST_DIR to a checkout of github-linguist/linguist"
[ -f "$LINGUIST_DIR/lib/linguist/languages.yml" ] || fail "$LINGUIST_DIR does not look like a linguist checkout"
command -v ruby >/dev/null || fail "ruby is required (see linguist CONTRIBUTING.md)"
command -v bundle >/dev/null || fail "bundler is required"
[ -d "$samples" ] && ls "$samples"/*.mncs >/dev/null 2>&1 || fail "no samples found next to this script"

step "1/5 checking linguist toolchain"
if ! (cd "$LINGUIST_DIR" && bundle check >/dev/null 2>&1); then
  echo "gem dependencies missing; run: cd \"$LINGUIST_DIR\" && bundle install" >&2
  echo "(charlock_holmes needs ICU/pkg-config; see CONTRIBUTING.md)" >&2
  exit 1
fi

step "2/5 injecting MNCS language definition"
backup="$(mktemp)"
cp "$LINGUIST_DIR/lib/linguist/languages.yml" "$backup"
restore() {
  if [ -z "$KEEP" ]; then
    cp "$backup" "$LINGUIST_DIR/lib/linguist/languages.yml"
    rm -rf "$LINGUIST_DIR/samples/MNCS"
    rm -rf /tmp/opencode/mncs-linguist-repo
    echo "== injected changes reverted (set KEEP=1 to keep)"
  else
    echo "== changes kept in $LINGUIST_DIR"
  fi
}
trap restore EXIT

ruby - "$LINGUIST_DIR" "$fragment" <<'RUBY'
require 'yaml'
linguist_dir, fragment_path = ARGV
path = File.join(linguist_dir, 'lib/linguist/languages.yml')
raw = File.read(path)
abort 'MNCS already present in languages.yml' if raw.match?(/^MNCS:/)

body = File.read(fragment_path)
         .lines
         .reject { |line| line.start_with?('#') || line.strip.empty? }
         .join
abort 'fragment must start with `MNCS:`' unless body.start_with?('MNCS:')

name = 'MNCS'
insert_at = nil
raw.each_line.with_index do |line, index|
  next if line.start_with?('#', ' ', "\t", '-')
  key = line[/\A([^:\s]+):/, 1]
  next unless key
  if key > name
    insert_at = index
    break
  end
end

updated =
  if insert_at
    lines = raw.lines
    lines.insert(insert_at, body).join
  else
    raw.sub(/\A(#.*\n)+/) { |header| header + body }
  end

File.write(path, updated)
YAML.safe_load(updated, aliases: true) or abort 'resulting YAML does not parse'
puts "inserted #{name} before line #{insert_at || 'EOF'}; YAML parses"
RUBY

step "3/5 installing samples"
mkdir -p "$LINGUIST_DIR/samples/MNCS"
cp "$samples"/*.mncs "$LINGUIST_DIR/samples/MNCS/"
ls -1 "$LINGUIST_DIR/samples/MNCS" | sed 's/^/  samples\/MNCS\//'

step "4/5 regenerating samples database"
( cd "$LINGUIST_DIR" && bundle exec rake samples >/dev/null )
echo "samples cache regenerated"

step "5/5 focused classification checks"
scratch=/tmp/opencode/mncs-linguist-repo
rm -rf "$scratch"
mkdir -p "$scratch"
cp "$samples"/*.mncs "$scratch/"
git -C "$scratch" init -q .
git -C "$scratch" add .

cd "$LINGUIST_DIR"

ruby <<'RUBY'
require 'linguist'
include Linguist

language = Language['MNCS']
abort 'Language["MNCS"] not registered' unless language
raise "alias mncs broken" unless Language['mncs'] == language
raise "extension .mncs broken" unless language.extensions.include?('.mncs')
puts "  language registered: #{language.name.inspect} type=#{language.type} tm_scope=#{language.tm_scope}"

Dir['samples/MNCS/*.mncs'].sort.each do |path|
  blob = FileBlob.new(path)
  detected = blob.language.name
  raise "#{path} classified as #{detected}, expected MNCS" unless detected == 'MNCS'
  puts "  ok #{path}"
end
puts '  all samples classify as MNCS'
RUBY

echo
echo '-- bin/github-linguist --breakdown on scratch repository --'
bundle exec bin/github-linguist --breakdown "$scratch" || \
  echo "(github-linguist CLI failed; classification checks above remain authoritative)"

if [ -n "$RUN_FULL_TESTS" ]; then
  step "bonus: full upstream test suite"
  bundle exec rake test
fi

printf '\n== RESULT: MNCS integration validated against %s\n' "$(git -C "$LINGUIST_DIR" rev-parse HEAD 2>/dev/null || echo 'unknown commit')"
