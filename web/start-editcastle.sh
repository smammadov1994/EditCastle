#!/usr/bin/env bash
set -euo pipefail
umask 077
cd "$(dirname "$0")"
if ! command -v cargo >/dev/null; then
    if [[ -x /opt/homebrew/bin/rustup ]]; then
        export PATH="$(/opt/homebrew/bin/rustup which cargo | xargs dirname):$PATH"
    fi
fi
command -v cargo >/dev/null || { printf 'Install Rust before starting EditCastle.\n' >&2; exit 1; }
command -v ffmpeg >/dev/null || { printf 'Install FFmpeg before starting EditCastle.\n' >&2; exit 1; }
command -v ffprobe >/dev/null || { printf 'Install FFprobe before starting EditCastle.\n' >&2; exit 1; }
data="${EDITROOM_DATA:-studio-data}"
if [[ -z "${EDITROOM_SUPABASE_URL:-}" && -z "${EDITROOM_MCP_KEY_FILE:-}" && -s "$data/mcp-key" ]]; then
    export EDITROOM_MCP_KEY_FILE="$data/mcp-key"
fi
cargo build --locked
if [[ "${1:-}" != "--open" ]]; then
    exec target/debug/editcastle-web
fi
if [[ "$(uname -s)" != "Darwin" ]]; then
    printf 'Run the server without --open, then sign in from your browser.\n' >&2
    exit 1
fi
address="${EDITROOM_BIND:-127.0.0.1:4324}"
target/debug/editcastle-web &
server=$!
trap 'kill "$server" 2>/dev/null || true' EXIT
for _ in {1..50}; do
    if [[ -n "${EDITROOM_SUPABASE_URL:-}" || -s "$data/access-key" ]] && curl -fsS -o /dev/null "http://$address/studio"; then
        if [[ -n "${EDITROOM_SUPABASE_URL:-}" ]]; then
            /usr/bin/open "http://$address/studio"
        else
            /usr/bin/open "http://$address/studio#sign-in=$(< "$data/access-key")"
        fi
        wait "$server"
        exit $?
    fi
    kill -0 "$server" 2>/dev/null || exit 1
    sleep .2
done
printf 'The EditCastle server did not become ready.\n' >&2
exit 1
