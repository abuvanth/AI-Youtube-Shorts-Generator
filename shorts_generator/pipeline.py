"""End-to-end orchestrator.

Two modes:
  * mode="local" (default) — yt-dlp + ElevenLabs ASR (or Whisper) + Gemini (gemini-4-hydrogen with video understanding) + ffmpeg/opencv.
  * mode="api"             — MuAPI does download / transcribe / LLM / autocrop.
"""
import os
from typing import Dict, List, Optional

from .clipper import crop_highlights
from .config import (
    LLM_PROVIDER,
    MUAPI_API_KEY,
    _PLACEHOLDER_KEY,
    require_api_key,
    require_gemini_key,
    require_openai_key,
)
from .downloader import download_youtube
from .highlights import call_muapi_llm, get_highlights
from .transcriber import transcribe


def _run_local(
    youtube_url: str,
    num_clips: int,
    aspect_ratio: str,
    download_format: str,
    language: Optional[str],
    analysis_format: str = "360",
) -> Dict:
    from .local.clipper import crop_highlights_local
    from .local.downloader import download_youtube_local
    from .local.llm import call_local_llm
    from .local.transcriber import transcribe_local

    # Stage 1: Fast virality check & ASR using low-res preview file (e.g. 360p)
    # This prevents uploading massive multi-hundred-MB files to Gemini API.
    print(f"[pipeline/local] fetching low-res preview ({analysis_format}p) for fast transcription and virality check...", flush=True)
    analysis_path = download_youtube_local(youtube_url, fmt=analysis_format)

    transcript = transcribe_local(analysis_path, language=language)
    if not transcript or not transcript.get("segments"):
        print("[pipeline/local] Warning: speech transcription produced 0 segments. Relying on Gemini video understanding.", flush=True)
        duration = transcript.get("duration", 0.0) if transcript else 0.0
        transcript = {"duration": max(duration, 30.0), "segments": [{"start": 0.0, "end": max(duration, 30.0), "text": "(visual content)"}]}

    highlights_result = get_highlights(transcript, num_clips=num_clips, llm_fn=call_local_llm, video_path=analysis_path)
    all_highlights: List[Dict] = highlights_result.get("highlights", [])
    if not all_highlights:
        raise RuntimeError("Highlight generator returned zero clips.")

    top = sorted(all_highlights, key=lambda h: int(h.get("score", 0)), reverse=True)[:num_clips]

    # Stage 2: Final Cut — Fetch/use Full HD source file (e.g. 1080p) exclusively for cutting the final clips
    if download_format != analysis_format and not os.path.exists(youtube_url):
        print(f"[pipeline/local] fetching HD video ({download_format}p) for final cut...", flush=True)
        hd_path = download_youtube_local(youtube_url, fmt=download_format)
    else:
        hd_path = analysis_path

    print(f"[pipeline/local] rendering {len(top)} Full HD shorts from {hd_path}...", flush=True)
    shorts = crop_highlights_local(hd_path, top, aspect_ratio=aspect_ratio)

    return {
        "mode": "local",
        "source_video_url": hd_path,
        "transcript": transcript,
        "highlights": all_highlights,
        "shorts": shorts,
    }


def _run_api(
    youtube_url: str,
    num_clips: int,
    aspect_ratio: str,
    download_format: str,
    language: Optional[str],
) -> Dict:
    source_url = download_youtube(youtube_url, fmt=download_format, use_muapi=True)

    transcript = transcribe(source_url, language=language)
    if not transcript["segments"]:
        raise RuntimeError(
            "Whisper produced no segments. The video may have no detectable speech."
        )

    highlights_result = get_highlights(transcript, num_clips=num_clips, llm_fn=call_muapi_llm)
    all_highlights: List[Dict] = highlights_result.get("highlights", [])
    if not all_highlights:
        raise RuntimeError("Highlight generator returned zero clips.")

    top = sorted(all_highlights, key=lambda h: int(h.get("score", 0)), reverse=True)[:num_clips]
    print(f"[pipeline] cropping {len(top)} of {len(all_highlights)} candidates", flush=True)

    shorts = crop_highlights(source_url, top, aspect_ratio=aspect_ratio)

    return {
        "mode": "api",
        "source_video_url": source_url,
        "transcript": transcript,
        "highlights": all_highlights,
        "shorts": shorts,
    }


def generate_shorts(
    youtube_url: str,
    num_clips: int = 3,
    aspect_ratio: str = "9:16",
    download_format: str = "1080",
    language: Optional[str] = None,
    mode: Optional[str] = None,
    analysis_format: str = "360",
) -> Dict:
    """Run the full pipeline and return a structured result.

    Args:
        youtube_url: source URL or local path.
        num_clips: how many shorts to render.
        aspect_ratio: e.g. "9:16", "1:1".
        download_format: source resolution ("360" / "480" / "720" / "1080").
        language: ISO-639-1 code (optional).
        mode: "local" (default: yt-dlp + ElevenLabs ASR + Gemini 4 Hydrogen + ffmpeg)
            or "api" (MuAPI). If omitted, automatically detects based on configured keys.

    Returns:
        {
          "mode": "local" | "api",
          "source_video_url": str,
          "transcript": {...},
          "highlights": [...],
          "shorts": [...],
        }
    """
    if not mode or mode == "auto":
        # Auto-detect mode: if MuAPI is configured, prefer api if requested, otherwise local
        has_muapi = bool(MUAPI_API_KEY and not _PLACEHOLDER_KEY.fullmatch(MUAPI_API_KEY) and len(MUAPI_API_KEY) >= 16)
        mode = "api" if (has_muapi and os.getenv("DEFAULT_MODE") == "api") else "local"

    mode = mode.lower()
    if mode == "local":
        provider = (LLM_PROVIDER or "gemini").strip().lower()
        if provider == "openai":
            require_openai_key()
        elif provider == "gemini":
            require_gemini_key()
        else:
            raise RuntimeError(f"Unknown LLM_PROVIDER={provider!r}. Use 'gemini' or 'openai'.")
        return _run_local(
            youtube_url,
            num_clips,
            aspect_ratio,
            download_format,
            language,
            analysis_format=analysis_format,
        )
    if mode == "api":
        require_api_key()
        return _run_api(youtube_url, num_clips, aspect_ratio, download_format, language)
    raise ValueError(f"Unknown mode: {mode!r}. Use 'local' or 'api'.")
