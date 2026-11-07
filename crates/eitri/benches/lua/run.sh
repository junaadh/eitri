#!/bin/sh
# Build and run the Lua 5.4 baseline for T011 (see harness.c).
# Needs Lua 5.4 headers and a static library; on macOS: `brew install lua@5.4`.
# Override the location with LUA_PREFIX.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../../../.." && pwd)
prefix=${LUA_PREFIX:-$(brew --prefix lua@5.4 2>/dev/null || echo /usr/local)}
out="$root/target/bench-results"
mkdir -p "$out"
cc -O2 -std=c11 -Wall -Wextra -I"$prefix/include/lua" -I"$prefix/include" \
    "$here/harness.c" "$prefix/lib/liblua.a" -lm -o "$out/lua_harness"
echo "cc: $(cc --version | head -1); flags: -O2; liblua.a from $prefix"
"$out/lua_harness" "$here/../fixtures" "${1:-20001}" "$out/lua.csv"
echo
echo "raw samples: $out/lua.csv"
