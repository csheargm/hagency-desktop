#!/usr/bin/env bash
# Put the pinned source checkouts of .jit/sources.lock under .sources/.
#
#   .jit/sources.sh           fetch what is missing, then check every pin
#   .jit/sources.sh --check   only check; fetch nothing
#
# A checkout that is already there is left alone, so a developer can point
# .sources/<name> at a working tree of their own (a symlink is fine). The
# script then only says whether that tree is at the pinned commit.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
lock="$root/.jit/sources.lock"
check_only=0
[ "${1:-}" = "--check" ] && check_only=1

status=0
while read -r name url rev; do
    case "$name" in ''|'#'*) continue ;; esac
    dir="$root/.sources/$name"
    if [ ! -e "$dir" ]; then
        if [ "$check_only" = 1 ]; then
            echo "missing  .sources/$name (pinned at $rev)"
            status=1
            continue
        fi
        echo "fetching .sources/$name at $rev"
        mkdir -p "$dir"
        git -C "$dir" init -q
        git -C "$dir" remote add origin "$url"
        git -C "$dir" fetch -q --depth 1 origin "$rev"
        git -C "$dir" -c advice.detachedHead=false checkout -q FETCH_HEAD
    fi
    head=$(git -C "$dir" rev-parse HEAD)
    if [ "$head" = "$rev" ]; then
        echo "ok       .sources/$name $rev"
    elif [ "$(git -C "$dir" rev-parse 'HEAD^{tree}')" = "$(git -C "$dir" rev-parse "$rev^{tree}" 2>/dev/null)" ]; then
        echo "ok       .sources/$name at $head, the same tree as the pin $rev"
    else
        echo "differs  .sources/$name is at $head, pinned at $rev"
        status=1
    fi
done < "$lock"
exit $status
