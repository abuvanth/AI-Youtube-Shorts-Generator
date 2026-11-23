"""Local clipping: ffmpeg subclip + vertical reframing for 9:16 Shorts/Reels."""
import os
import subprocess
from typing import Dict, List, Optional

from ..config import LOCAL_OUTPUT_DIR


def _ratio(aspect_ratio: str) -> float:
    """Parse '9:16' → 9/16, '1:1' → 1.0."""
    try:
        w, h = aspect_ratio.split(":")
        return float(w) / float(h)
    except (ValueError, ZeroDivisionError):
        return 9.0 / 16.0


def crop_clip_local(
    source_path: str,
    start_time: float,
    end_time: float,
    aspect_ratio: str,
    out_path: str,
) -> str:
    """Cut + reframe one highlight to target aspect ratio (e.g. 9:16 vertical standard 1080x1920)."""
    source_path = os.path.abspath(os.path.expanduser(source_path))
    out_path = os.path.abspath(os.path.expanduser(out_path))

    ratio = _ratio(aspect_ratio)
    # Target scale: for standard 9:16 Shorts/TikTok/Reels, output crisp 1080x1920 HD
    if abs(ratio - (9.0 / 16.0)) < 0.05:
        scale_target = "scale=1080:1920"
    elif abs(ratio - 1.0) < 0.05:
        scale_target = "scale=1080:1080"
    else:
        scale_target = "scale='trunc(iw/2)*2':'trunc(ih/2)*2'"

    # Crop to ratio, center frame, and scale to target resolution
    vf_filter = (
        f"crop='min(iw,ih*{ratio:.4f})':'min(ih,iw/{ratio:.4f})':"
        f"'(iw-ow)/2':'(ih-oh)/2',{scale_target}"
    )

    cmd = [
        "ffmpeg", "-y", "-loglevel", "error",
        "-ss", f"{start_time:.3f}",
        "-to", f"{end_time:.3f}",
        "-i", source_path,
        "-vf", vf_filter,
        "-c:v", "libx264", "-preset", "fast", "-crf", "18",
        "-c:a", "aac", "-b:a", "192k",
        out_path,
    ]
    subprocess.run(cmd, check=True)
    return out_path


def crop_highlights_local(
    source_path: str,
    highlights: List[Dict],
    aspect_ratio: str = "9:16",
    out_dir: Optional[str] = None,
) -> List[Dict]:
    """Render each highlight candidate as a vertical short."""
    out_dir = os.path.abspath(os.path.expanduser(out_dir or LOCAL_OUTPUT_DIR))
    os.makedirs(out_dir, exist_ok=True)
    results: List[Dict] = []
    for i, h in enumerate(highlights, 1):
        out_path = os.path.join(out_dir, f"short_{i:02d}.mp4")
        print(f"[clip/local] {i}/{len(highlights)}: {h.get('title', '(untitled)')}", flush=True)
        try:
            crop_clip_local(
                source_path,
                float(h["start_time"]),
                float(h["end_time"]),
                aspect_ratio,
                out_path,
            )
            results.append({**h, "clip_url": out_path})
        except Exception as e:
            print(f"[clip/local] {i} failed: {e}", flush=True)
            results.append({**h, "clip_url": None, "error": str(e)})
    return results
