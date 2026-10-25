#!/bin/sh
# Starts the FigCraft MCP server for Claude Code without making the client wait for a compile.
# Modelled on EffectCraft's scripts/mcp.sh.
#
# figcraft-cli is built with the `local` profile (release without LTO, parallel codegen,
# incremental) in target/local, the folder the Dock app's build uses (scripts/fork/app.sh), so a
# rebuild only recompiles what changed.
#
# - Sources newer than the binary: the existing binary starts at once and one rebuild runs in the
#   background (a lock; a lock left by a build that died is cleared). Reconnect (/mcp) to use it.
# - No binary yet: the first start compiles in the foreground.
#
# Usage: scripts/fork/mcp.sh [figcraft-cli mcp arguments], e.g. `--attach 7979` for the running app.
set -u

PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"
CARGO_PROFILE_LOCAL_INHERITS=release CARGO_PROFILE_LOCAL_LTO=false CARGO_PROFILE_LOCAL_CODEGEN_UNITS=16 CARGO_PROFILE_LOCAL_INCREMENTAL=true
export CARGO_PROFILE_LOCAL_INHERITS CARGO_PROFILE_LOCAL_LTO CARGO_PROFILE_LOCAL_CODEGEN_UNITS CARGO_PROFILE_LOCAL_INCREMENTAL

root=$(cd "$(dirname "$0")/../.." && pwd)
target="$root/target/local"
bin="$target/local/figcraft-cli"
lock="$target/mcp-build.lock"
log="$target/mcp-build.log"

build() {
    CARGO_TARGET_DIR="$target" CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}" nice -n 10 cargo build --quiet --profile local -p figcraft-cli --manifest-path "$root/Cargo.toml"
}

take_lock() {
    if ! mkdir "$lock" 2>/dev/null; then
        owner=$(cat "$lock/pid" 2>/dev/null || echo "")
        if [ -n "$owner" ] && kill -0 "$owner" 2>/dev/null; then
            return 1
        fi
        if [ -z "$owner" ] && [ -n "$(find "$lock" -maxdepth 0 -mmin -1 2>/dev/null)" ]; then
            return 1
        fi
        rm -rf "$lock"
        mkdir "$lock" 2>/dev/null || return 1
    fi
    return 0
}

stale() {
    [ -n "$(find "$root/crates" "$root/apps/figcraft-cli" "$root/Cargo.toml" "$root/Cargo.lock" -newer "$bin" \( -name '*.rs' -o -name '*.toml' -o -name '*.lock' -o -name '*.json' -o -name '*.wgsl' \) -print 2>/dev/null | head -n 1)" ]
}

mkdir -p "$target"
if [ ! -x "$bin" ]; then
    echo "figcraft mcp: first start, compiling figcraft-cli (if the client times out, wait for this and reconnect)" >&2
    build >"$log" 2>&1 || { cat "$log" >&2; exit 1; }
elif stale; then
    if take_lock; then
        echo "figcraft mcp: sources changed, rebuilding in the background; reconnect to use the new build" >&2
        (
            build >"$log" 2>&1
            rm -rf "$lock"
        ) </dev/null >/dev/null 2>&1 &
        echo "$!" >"$lock/pid" 2>/dev/null
    fi
fi

exec "$bin" mcp "$@"
