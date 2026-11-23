"""ElevenLabs Speech-to-Text (ASR) backend using Scribe API."""
import os
import re
import shutil
import subprocess
from pathlib import Path
from typing import Dict, List, Optional

import requests

from .config import (
    ELEVENLABS_SCRIBE_MODEL,
    LOCAL_OUTPUT_DIR,
    require_elevenlabs_key,
)


def _transcript_cache_path(media_path: str) -> Path:
    """Return the .srt cache path for a media file, normalizing resolution suffixes."""
    cache_dir = Path(LOCAL_OUTPUT_DIR)
    cache_dir.mkdir(parents=True, exist_ok=True)
    raw_stem = Path(media_path).stem
    # If exact cache exists, use it
    if (cache_dir / f"{raw_stem}.srt").exists():
        return cache_dir / f"{raw_stem}.srt"
    # Otherwise check normalized stem without resolution suffix
    base_stem = re.sub(r"_(?:360|480|720|1080|1440|2160)$", "", raw_stem)
    return cache_dir / f"{base_stem}.srt"


def _format_srt_timestamp(seconds: float) -> str:
    total_ms = max(0, int(round(seconds * 1000)))
    ms = total_ms % 1000
    total_s = total_ms // 1000
    s = total_s % 60
    total_m = total_s // 60
    m = total_m % 60
    h = total_m // 60
    return f"{h:02d}:{m:02d}:{s:02d},{ms:03d}"


def _parse_srt_timestamp(value: str) -> float:
    match = re.fullmatch(r"(\d{2}):(\d{2}):(\d{2}),(\d{3})", value.strip())
    if not match:
        raise ValueError(f"Invalid SRT timestamp: {value!r}")
    hours, minutes, seconds, millis = map(int, match.groups())
    return hours * 3600 + minutes * 60 + seconds + (millis / 1000.0)


def _write_srt_cache(media_path: str, transcript: Dict) -> Path:
    cache_path = _transcript_cache_path(media_path)
    lines = []
    for idx, segment in enumerate(transcript.get("segments", []), start=1):
        start = _format_srt_timestamp(float(segment["start"]))
        end = _format_srt_timestamp(float(segment["end"]))
        text = str(segment.get("text", "")).strip().replace("\r", "").replace("\n", " ")
        lines.append(str(idx))
        lines.append(f"{start} --> {end}")
        lines.append(text)
        lines.append("")

    cache_path.write_text("\n".join(lines), encoding="utf-8")
    return cache_path


def _load_srt_cache(cache_path: Path) -> Dict:
    content = cache_path.read_text(encoding="utf-8-sig").strip()
    if not content:
        return {"duration": 0.0, "segments": []}

    segments = []
    for block in re.split(r"\n\s*\n", content):
        lines = [line.strip("\ufeff") for line in block.splitlines() if line.strip()]
        if not lines:
            continue
        if "-->" not in lines[0] and len(lines) > 1 and "-->" in lines[1]:
            lines = lines[1:]
        if not lines or "-->" not in lines[0]:
            continue
        start_raw, end_raw = [part.strip() for part in lines[0].split("-->", 1)]
        text = "\n".join(lines[1:]).strip()
        segments.append(
            {
                "start": _parse_srt_timestamp(start_raw),
                "end": _parse_srt_timestamp(end_raw),
                "text": text,
            }
        )

    duration = segments[-1]["end"] if segments else 0.0
    return {"duration": duration, "segments": segments}


def _extract_audio_for_asr(media_path: str) -> str:
    """Extract a lightweight MP3 audio track from media to save upload bandwidth."""
    ext = Path(media_path).suffix.lower()
    if ext in (".mp3", ".wav", ".m4a", ".aac", ".flac", ".ogg"):
        return media_path

    out_dir = Path(LOCAL_OUTPUT_DIR)
    out_dir.mkdir(parents=True, exist_ok=True)
    audio_path = str(out_dir / (Path(media_path).stem + "_asr.mp3"))

    # If cached audio exists and is fresh, reuse
    if os.path.exists(audio_path) and os.path.getmtime(audio_path) >= os.path.getmtime(media_path):
        return audio_path

    cmd = [
        "ffmpeg", "-y", "-loglevel", "error",
        "-i", media_path,
        "-vn",
        "-acodec", "libmp3lame",
        "-ar", "16000",
        "-ac", "1",
        "-b:a", "64k",
        audio_path,
    ]
    try:
        subprocess.run(cmd, check=True)
        return audio_path
    except Exception as e:
        print(f"[elevenlabs_asr] audio extraction via ffmpeg failed ({e}), uploading source directly", flush=True)
        return media_path


def _words_to_segments(words: List[Dict], full_text: str, duration: float) -> List[Dict]:
    """Group word-level timestamps into natural sentences and segments."""
    if not words:
        if full_text and duration > 0:
            return [{"start": 0.0, "end": duration, "text": full_text.strip()}]
        return []

    segments: List[Dict] = []
    current_words: List[str] = []
    seg_start: Optional[float] = None
    seg_end: Optional[float] = None

    for item in words:
        w_text = str(item.get("text", "")).strip()
        if not w_text:
            continue

        w_start = float(item.get("start", 0.0))
        w_end = float(item.get("end", w_start))

        if seg_start is None:
            seg_start = w_start

        # Check for pause gap > 1.2s
        if seg_end is not None and (w_start - seg_end) > 1.2 and current_words:
            segments.append({
                "start": round(seg_start, 2),
                "end": round(seg_end, 2),
                "text": " ".join(current_words),
            })
            current_words = []
            seg_start = w_start

        current_words.append(w_text)
        seg_end = w_end

        # Sentence end punctuation or segment length >= 18 words or length >= 8 seconds
        is_sentence_end = bool(re.search(r"[.?!…]+$", w_text))
        is_too_long = len(current_words) >= 18 or (seg_end - seg_start) >= 8.0

        if (is_sentence_end or is_too_long) and current_words:
            segments.append({
                "start": round(seg_start, 2),
                "end": round(seg_end, 2),
                "text": " ".join(current_words),
            })
            current_words = []
            seg_start = None
            seg_end = None

    if current_words and seg_start is not None and seg_end is not None:
        segments.append({
            "start": round(seg_start, 2),
            "end": round(seg_end, 2),
            "text": " ".join(current_words),
        })

    return segments


def transcribe_elevenlabs(media_path: str, language: Optional[str] = None) -> Dict:
    """Run ElevenLabs Speech-to-Text on a media file and return {duration, segments}."""
    cache_path = _transcript_cache_path(media_path)
    if cache_path.exists():
        source_mtime = os.path.getmtime(media_path)
        cache_mtime = cache_path.stat().st_mtime
        if cache_mtime >= source_mtime:
            cached = _load_srt_cache(cache_path)
            if cached.get("segments") and cached.get("duration", 0) > 0:
                print(f"[elevenlabs_asr] reusing cached transcript: {cache_path}", flush=True)
                return cached
            else:
                cache_path.unlink(missing_ok=True)

    api_key = require_elevenlabs_key()
    audio_path = _extract_audio_for_asr(media_path)

    print(f"[elevenlabs_asr] uploading {audio_path} to ElevenLabs Scribe API...", flush=True)
    url = "https://api.elevenlabs.io/v1/speech-to-text"
    headers = {"xi-api-key": api_key}
    data = {
        "model_id": ELEVENLABS_SCRIBE_MODEL or "scribe_v1",
        "tag_audio_events": "false",
    }
    if language:
        data["language_code"] = language

    with open(audio_path, "rb") as f:
        files = {"file": (os.path.basename(audio_path), f, "audio/mpeg")}
        resp = requests.post(url, headers=headers, data=data, files=files, timeout=300)

    if resp.status_code >= 400:
        raise RuntimeError(f"ElevenLabs ASR failed [{resp.status_code}]: {resp.text}")

    result = resp.json()
    full_text = result.get("text", "")
    words = result.get("words", [])
    duration = float(result.get("audio_duration_secs", 0.0))

    segments = _words_to_segments(words, full_text, duration)
    if not duration and segments:
        duration = segments[-1]["end"]

    print(f"[elevenlabs_asr] transcribed {len(segments)} segments ({duration:.1f}s)", flush=True)
    transcript = {"duration": duration, "segments": segments}
    _write_srt_cache(media_path, transcript)
    return transcript
