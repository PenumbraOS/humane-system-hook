#!/usr/bin/env bash
set -euo pipefail

base_url="${1:-http://127.0.0.1:8080}"
base_url="${base_url%/}"
track_id="${TRACK_ID:-mock-1}"
tmp_dir="$(mktemp -d /tmp/penumbra-tidal-e2e.XXXXXX)"
trap 'rm -rf "$tmp_dir"' EXIT

request() {
  local name="$1" url="$2"
  curl --fail-with-body -sS --max-time 15 "$url" >"$tmp_dir/$name"
  printf 'OK  %s\n' "$url"
}

request featured "$base_url/tidal-shim/v1/featured/recommended/playlists"
request playlist "$base_url/tidal-shim/v1/playlists/poc-playlist/items"
request search "$base_url/tidal-shim/v1/search/top-hits/?query=${SEARCH_TERM:-test}"
request track "$base_url/tidal-shim/v1/tracks/$track_id"
request recommendations "$base_url/tidal-shim/v1/tracks/$track_id/recommendations"
request playback "$base_url/tidal-shim/v1/tracks/$track_id/playbackinfopostpaywall"

manifest="$(jq -r '.manifest' "$tmp_dir/playback")"
printf '%s' "$manifest" | base64 --decode >"$tmp_dir/manifest.json"
stream_url="$(jq -r '.urls[0]' "$tmp_dir/manifest.json")"
[[ "$stream_url" != "null" && -n "$stream_url" ]]
# The built-in mock advertises loopback because the player runs on-device. For
# an external harness, map that host to the server under test.
if [[ "$stream_url" == http://127.0.0.1:* ]]; then
  stream_url="${stream_url/http:\/\/127.0.0.1:8080/$base_url}"
fi

headers="$(curl --fail-with-body -sS -D - -o "$tmp_dir/range.bin" \
  -H 'Range: bytes=0-255' --max-time 15 "$stream_url")"
grep -qi '^HTTP/.*206' <<<"$headers"
grep -qi '^content-type: audio/wav' <<<"$headers"
grep -qi '^accept-ranges: bytes' <<<"$headers"
[[ "$(wc -c <"$tmp_dir/range.bin" | tr -d ' ')" -gt 0 ]]
printf 'OK  range playback (%s)\n' "$stream_url"
