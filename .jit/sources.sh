#!/usr/bin/env bash
# Put the pinned source checkouts of .jit/sources.lock under .sources/.
#
#   .jit/sources.sh             fetch what is missing, then check every pin
#   .jit/sources.sh --outside   the same, but keep the checkouts beside this
#                               repository and link .sources/<name> to them
#   .jit/sources.sh --check     only check; fetch nothing
#
# A checkout that is already there is left alone, so a developer can point
# .sources/<name> at a working tree of their own (a symlink is fine). The
# script then only says whether that tree is at the pinned commit.
#
# `makepad run` needs --outside: the JIT takes every path crate that lies
# inside this repository's folder for app code, and a makepad checkout that is
# a real directory under .sources/ then fails to compile (.jit/NOTES.md,
# "Setup"). A cargo build works either way. With --outside the checkouts go to
# $HAGENCY_SOURCES_DIR, or to ../<this folder's name>-sources.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
lock="$root/.jit/sources.lock"
check_only=0
outside=0
case "${1:-}" in
    --check) check_only=1 ;;
    --outside) outside=1 ;;
    '') ;;
    *) echo "usage: .jit/sources.sh [--outside | --check]" >&2; exit 2 ;;
esac
store="${HAGENCY_SOURCES_DIR:-$(dirname "$root")/$(basename "$root")-sources}"

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
        into="$dir"
        if [ "$outside" = 1 ]; then
            into="$store/$name"
            mkdir -p "$root/.sources"
        fi
        if [ ! -e "$into" ]; then
            echo "fetching ${into#"$root"/} at $rev"
            mkdir -p "$into"
            git -C "$into" init -q
            git -C "$into" remote add origin "$url"
            git -C "$into" fetch -q --depth 1 origin "$rev"
            git -C "$into" -c advice.detachedHead=false checkout -q FETCH_HEAD
        fi
        if [ "$outside" = 1 ]; then
            ln -s "$into" "$dir"
            echo "linked   .sources/$name -> $into"
        fi
    fi
    if [ ! -L "$dir" ] && [ -z "${CI:-}" ]; then
        echo "note     .sources/$name is a directory inside this repository: fine for cargo," \
            "but \`makepad run\` needs it outside (move it away and run .jit/sources.sh --outside)"
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
