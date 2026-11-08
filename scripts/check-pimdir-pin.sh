#!/usr/bin/env bash
# Compares the io-pimdir rev Rail pins (the workspace Cargo.toml) with the revs neverest and
# himalaya pin on their default branches. A store opens under one spec draft alone, so Rail must
# read stores with the io-pimdir the user's neverest writes them with.
#
#   scripts/check-pimdir-pin.sh          # exit 0 when all three agree, 1 when one drifted
#
# Needs curl and network access. Run it before bumping io-pimdir, and when neverest releases.

set -euo pipefail

cd "$(dirname "$0")/.."

# The rev of `io-pimdir = { git = "...io-pimdir", rev = "<rev>" ... }` in a manifest on stdin.
rev_of() { grep -E '^io-pimdir = \{ git = "https://github.com/pimalaya/io-pimdir"' | sed -E 's/.*rev = "([0-9a-f]+)".*/\1/'; }

ours=$(rev_of < Cargo.toml || true)
[ -n "$ours" ] || { echo "no io-pimdir git pin in Cargo.toml" >&2; exit 1; }
echo "rail      $ours"

status=0
for project in neverest himalaya; do
    theirs=$(curl -fsSL "https://raw.githubusercontent.com/pimalaya/$project/HEAD/Cargo.toml" | rev_of || true)
    if [ -z "$theirs" ]; then
        echo "$project  (no io-pimdir git pin found; check its Cargo.toml by hand)"
        status=1
    elif [ "$theirs" = "$ours" ]; then
        echo "$project  $theirs  same"
    else
        echo "$project  $theirs  DIFFERS"
        status=1
    fi
done
exit $status
