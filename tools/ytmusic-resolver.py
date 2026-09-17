#!/usr/bin/env python3
"""Small LAN resolver for the Penumbra YouTube Music provider.

It deliberately keeps the provider contract simple: JSON metadata plus cached
WAV files with HTTP byte-range support. It uses the installed yt-dlp and ffmpeg
commands, so no API key or login is required for public searches.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import shutil
import tempfile
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlparse


ROOT = Path(__file__).resolve().parent.parent
CACHE = Path(os.environ.get("YTMUSIC_CACHE", ROOT / ".ytmusic-cache"))
YTDLP = os.environ.get("YTDLP", "yt-dlp")


def run_json(*args: str) -> dict:
    result = subprocess.run(
        [YTDLP, *args], check=True, capture_output=True, text=True, timeout=60
    )
    return json.loads(result.stdout)


def track_from_entry(entry: dict) -> dict:
    video_id = entry.get("id") or entry.get("url")
    return {
        "id": video_id,
        "title": entry.get("title") or video_id,
        "artist": entry.get("uploader") or entry.get("channel") or "YouTube Music",
        "album": entry.get("album") or "YouTube Music",
        "duration_ms": int((entry.get("duration") or 0) * 1000),
    }


VIDEO_ID = re.compile(r"^[A-Za-z0-9_-]{11}$")


def is_video_entry(entry: dict) -> bool:
    """Whether a flat-playlist entry is a real, streamable video.

    `ytsearch` mixes channels and playlists in with videos. Only a video has an
    11-character id and the plain `Youtube` extractor; a channel id (`UC…`) is
    24 characters and carries `YoutubeTab`. Letting one through looks fine all
    the way to the device, which then fails to stream it with a 502.
    """
    if entry.get("ie_key") not in (None, "Youtube"):
        return False
    if entry.get("_type") not in (None, "url", "video"):
        return False
    return bool(VIDEO_ID.match(entry.get("id") or ""))


def search(term: str, limit: int = 6) -> list[dict]:
    # Over-fetch: the non-video entries dropped below would otherwise eat into
    # the limit and return a short (or empty) list for a valid search.
    data = run_json("--flat-playlist", "--dump-single-json", f"ytsearch{limit * 3}:{term}")
    entries = [entry for entry in data.get("entries", []) if is_video_entry(entry)]
    return [track_from_entry(entry) for entry in entries[:limit]]


def metadata(video_id: str) -> dict:
    """Exact metadata for one video id (not a text search over the id)."""
    data = run_json(
        "--no-playlist", "--dump-single-json",
        f"https://www.youtube.com/watch?v={video_id}",
    )
    return track_from_entry(data)


def related(seed_id: str, limit: int = 6) -> list[dict]:
    """Tracks related to a seed, for the device's "up next" queue.

    yt-dlp has no cheap related-videos feed, so this searches on the seed's own
    artist and drops the seed itself. A generic search here instead would put
    unrelated artists into the queue — and the app will happily play them.
    """
    try:
        seed = metadata(seed_id)
    except (subprocess.SubprocessError, json.JSONDecodeError, KeyError, IndexError):
        return search("popular music", limit)

    term = seed.get("artist") or seed.get("title") or "popular music"
    tracks = [t for t in search(term, limit + 1) if t["id"] != seed_id]
    return tracks[:limit]


def cached_wav(video_id: str) -> Path:
    CACHE.mkdir(parents=True, exist_ok=True)
    target = CACHE / f"{video_id}.wav"
    if target.exists() and target.stat().st_size > 44:
        return target
    temp_dir = Path(tempfile.mkdtemp(prefix=f"{video_id}-", dir=CACHE))
    temp_path = temp_dir / "audio.wav"
    try:
        subprocess.run(
            [YTDLP, "--no-playlist", "-x", "--audio-format", "wav",
             # Prefer any real audio stream, and use player clients that avoid the
             # android_vr 403 / PO-token path YouTube now gates some videos behind.
             "-f", "bestaudio/best",
             "--extractor-args", "youtube:player_client=web_safari,mweb,tv",
             "-o", str(temp_dir / "audio.%(ext)s"),
             f"https://www.youtube.com/watch?v={video_id}"],
            check=True, timeout=300,
        )
        produced = next(temp_dir.glob("audio.*"))
        produced.rename(temp_path)
        temp_path.replace(target)
        return target
    finally:
        shutil.rmtree(temp_dir, ignore_errors=True)


class ResolverHandler(BaseHTTPRequestHandler):
    server_version = "PenumbraYtResolver/0.1"

    def log_message(self, fmt: str, *args: object) -> None:
        print(fmt % args, flush=True)

    def send_json(self, value: object, status: int = 200) -> None:
        payload = json.dumps(value).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def do_GET(self) -> None:  # noqa: N802
        parsed = urlparse(self.path)
        try:
            if parsed.path == "/health":
                return self.send_json({"status": "ok", "cache": str(CACHE)})
            if parsed.path == "/search":
                term = parse_qs(parsed.query).get("q", [""])[0]
                return self.send_json({"tracks": search(term or "popular music")})
            if parsed.path == "/queue":
                return self.send_json({"tracks": search("popular music")})
            if parsed.path == "/track/" or parsed.path.startswith("/track/"):
                video_id = parsed.path.rsplit("/", 1)[-1]
                return self.send_json(metadata(video_id))
            if parsed.path == "/recommendations" or parsed.path.startswith("/recommendations/"):
                seed = parsed.path.rsplit("/", 1)[-1]
                return self.send_json({"tracks": related(seed)})
            if parsed.path.startswith("/stream/"):
                return self.stream(parsed.path.rsplit("/", 1)[-1])
            return self.send_json({"error": "not found"}, 404)
        except (subprocess.SubprocessError, json.JSONDecodeError, IndexError, KeyError) as error:
            self.send_json({"error": str(error)}, 502)

    def stream(self, video_id: str) -> None:
        path = cached_wav(video_id)
        total = path.stat().st_size
        start, end = 0, total - 1
        range_header = self.headers.get("Range")
        if range_header and range_header.startswith("bytes="):
            raw = range_header[6:].split(",", 1)[0]
            left, right = raw.split("-", 1)
            start = int(left or 0)
            end = min(int(right) if right else end, total - 1)
            if start > end or start >= total:
                self.send_response(416)
                self.send_header("Content-Range", f"bytes */{total}")
                self.end_headers()
                return
        length = end - start + 1
        self.send_response(206 if range_header else 200)
        self.send_header("Content-Type", "audio/wav")
        self.send_header("Accept-Ranges", "bytes")
        self.send_header("Content-Length", str(length))
        if range_header:
            self.send_header("Content-Range", f"bytes {start}-{end}/{total}")
        self.end_headers()
        with path.open("rb") as audio:
            audio.seek(start)
            remaining = length
            while remaining:
                chunk = audio.read(min(1024 * 1024, remaining))
                if not chunk:
                    break
                self.wfile.write(chunk)
                remaining -= len(chunk)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--host", default="0.0.0.0")
    parser.add_argument("--port", type=int, default=8765)
    args = parser.parse_args()
    print(f"YouTube resolver listening on {args.host}:{args.port}", flush=True)
    ThreadingHTTPServer((args.host, args.port), ResolverHandler).serve_forever()


if __name__ == "__main__":
    main()
