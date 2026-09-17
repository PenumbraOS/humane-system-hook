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
FFMPEG = os.environ.get("FFMPEG", "ffmpeg")

# A song is not two hours long. Generic searches ("popular music") return
# multi-hour compilation videos, and this resolver downloads a track in full
# before serving any of it — so one such result pulled 1.5GB from YouTube in a
# single request. That is both the wrong content and what gets the client
# bot-flagged. Anything longer than this is not a track.
MAX_TRACK_SECONDS = int(os.environ.get("MAX_TRACK_SECONDS", "720"))

# Browser to lift YouTube cookies from ("chrome", "safari", "firefox", ...).
# Unset means anonymous requests, which YouTube increasingly answers with
# "Sign in to confirm you're not a bot". Set it and yt-dlp uses the signed-in
# session from that browser.
COOKIES_BROWSER = os.environ.get("YTDLP_COOKIES_BROWSER", "").strip()


def cookie_args() -> list[str]:
    return ["--cookies-from-browser", COOKIES_BROWSER] if COOKIES_BROWSER else []


def run_json(*args: str) -> dict:
    result = subprocess.run(
        [YTDLP, *cookie_args(), *args], check=True, capture_output=True, text=True, timeout=60
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
    if not VIDEO_ID.match(entry.get("id") or ""):
        return False
    # A missing duration is itself a signal: ordinary song results carry one,
    # while the endless "background music" compilations and live streams that
    # generic searches surface frequently do not. Requiring it costs a few real
    # results and blocks the ones that pull gigabytes.
    duration = entry.get("duration")
    if not duration or duration > MAX_TRACK_SECONDS:
        return False
    return True


def search(term: str, limit: int = 6) -> list[dict]:
    # Over-fetch generously. Generic terms ("popular music") are dominated by
    # hour-long compilations, which the filter drops — at 3x a queue request
    # came back with a single track. Named searches are unaffected; they just
    # fill from the first few results.
    data = run_json("--flat-playlist", "--dump-single-json", f"ytsearch{limit * 8}:{term}")
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


# `format` query value -> (file extension, yt-dlp --audio-format, content type)
FORMATS = {
    "wav": ("wav", "wav", "audio/wav"),
    "m4a": ("m4a", "m4a", "audio/mp4"),
}
DEFAULT_FORMAT = "wav"


def cached_audio(video_id: str, fmt: str = DEFAULT_FORMAT) -> tuple[Path, str]:
    """Return a cached audio file for `video_id`, downloading it if needed."""
    ext, audio_format, content_type = FORMATS.get(fmt, FORMATS[DEFAULT_FORMAT])
    CACHE.mkdir(parents=True, exist_ok=True)
    target = CACHE / f"{video_id}.{ext}"
    if target.exists() and target.stat().st_size > 44:
        return target, content_type

    # Transcode from a WAV we already hold rather than fetching again: it is
    # far faster, and YouTube rate-limits repeat downloads ("Sign in to confirm
    # you're not a bot") long before the cache goes cold.
    source = CACHE / f"{video_id}.wav"
    if fmt != "wav" and source.exists() and source.stat().st_size > 44:
        # The extension must stay correct: ffmpeg picks the container from it,
        # and a ".part" suffix makes it fail with "Unable to find a suitable
        # output format".
        tmp = CACHE / f"{video_id}.part.{ext}"
        subprocess.run(
            [FFMPEG, "-nostdin", "-y", "-i", str(source),
             "-c:a", "aac", "-b:a", "128k", str(tmp)],
            check=True, timeout=300,
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
        tmp.replace(target)
        return target, content_type

    temp_dir = Path(tempfile.mkdtemp(prefix=f"{video_id}-", dir=CACHE))
    temp_path = temp_dir / f"audio.{ext}"
    try:
        subprocess.run(
            [YTDLP, *cookie_args(),
             # Belt and braces: metadata can omit duration, and without this a
             # compilation still downloads in full.
             "--match-filter", f"duration < {MAX_TRACK_SECONDS}",
             "--no-playlist", "-x", "--audio-format", audio_format,
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
        return target, content_type
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
                fmt = parse_qs(parsed.query).get("format", [DEFAULT_FORMAT])[0]
                return self.stream(parsed.path.rsplit("/", 1)[-1], fmt)
            return self.send_json({"error": "not found"}, 404)
        except (subprocess.SubprocessError, json.JSONDecodeError, IndexError, KeyError) as error:
            self.send_json({"error": str(error)}, 502)

    def stream(self, video_id: str, fmt: str = DEFAULT_FORMAT) -> None:
        path, content_type = cached_audio(video_id, fmt)
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
        self.send_header("Content-Type", content_type)
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
