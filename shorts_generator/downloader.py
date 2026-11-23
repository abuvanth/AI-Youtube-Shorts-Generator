"""YouTube source video download via yt-dlp (default) or MuAPI."""
import os
from typing import Dict, Optional

from .local.downloader import download_youtube_local
from .url_validation import validate_youtube_url


def _extract_video_url(result: Dict) -> str:
    """MuAPI result shapes vary by endpoint — try common keys."""
    for key in ("video_url", "url", "output_url", "result_url"):
        v = result.get(key)
        if isinstance(v, str) and v.startswith("http"):
            return v

    output = result.get("outputs") or result.get("output") or result.get("result") or {}
    if isinstance(output, dict):
        for key in ("video_url", "url", "output_url"):
            v = output.get(key)
            if isinstance(v, str) and v.startswith("http"):
                return v
    if isinstance(output, list) and output and isinstance(output[0], str) and output[0].startswith("http"):
        return output[0]

    raise RuntimeError(f"Could not find downloaded video URL in MuAPI response: {result}")


def download_youtube_muapi(video_url: str, fmt: str = "720") -> str:
    """Hand a YouTube URL to MuAPI; return a hosted mp4 URL."""
    from . import muapi
    video_url = validate_youtube_url(video_url)
    print(f"[download/muapi] requesting {video_url} @ {fmt}p", flush=True)
    result = muapi.run(
        "youtube-download",
        {"video_url": video_url, "format": fmt},
        label="youtube-download",
    )
    out = _extract_video_url(result)
    print(f"[download/muapi] ready: {out}", flush=True)
    return out


def download_youtube(video_url: str, fmt: str = "720", use_muapi: bool = False) -> str:
    """Download YouTube video using yt-dlp by default, or MuAPI if requested."""
    if use_muapi:
        return download_youtube_muapi(video_url, fmt=fmt)
    # Default to yt-dlp
    return download_youtube_local(video_url, fmt=fmt)
