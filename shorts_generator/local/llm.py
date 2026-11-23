"""Local LLM backend — Gemini (default, gemini-4-hydrogen with video understanding) or OpenAI."""
import os
import time
from typing import Optional

from ..config import (
    GEMINI_MODEL,
    LLM_PROVIDER,
    OPENAI_MODEL,
    require_gemini_key,
    require_openai_key,
)


def call_openai_llm(prompt: str) -> str:
    """OpenAI Chat Completions backend used when LLM_PROVIDER=openai."""
    try:
        from openai import OpenAI  # type: ignore
    except ImportError as e:
        raise RuntimeError(
            "openai is required for LLM_PROVIDER=openai. Install it with:\n"
            "    pip install -r requirements-local.txt"
        ) from e

    client = OpenAI(api_key=require_openai_key())
    response = client.chat.completions.create(
        model=OPENAI_MODEL,
        temperature=0.7,
        messages=[{"role": "user", "content": prompt}],
    )
    return response.choices[0].message.content or ""


def call_gemini_llm(prompt: str, video_path: Optional[str] = None) -> str:
    """Gemini backend used by default (gemini-4-hydrogen).

    Supports multimodal Video Understanding per:
    https://aistudio.google.com/docs/video-understanding
    """
    try:
        from google import genai  # type: ignore
    except ImportError as e:
        raise RuntimeError(
            "google-genai is required for LLM_PROVIDER=gemini. Install it with:\n"
            "    pip install google-genai"
        ) from e

    client = genai.Client(api_key=require_gemini_key())
    uploaded_file = None

    contents = prompt
    if video_path and os.path.exists(video_path):
        try:
            file_size_mb = os.path.getsize(video_path) / (1024 * 1024)
            print(f"[gemini/video-understanding] uploading {video_path} ({file_size_mb:.1f} MB) to Files API...", flush=True)
            uploaded_file = client.files.upload(file=video_path)

            # Wait for video processing per Google AI Studio video understanding documentation
            while getattr(uploaded_file.state, "name", str(uploaded_file.state)) == "PROCESSING":
                print(".", end="", flush=True)
                time.sleep(3)
                uploaded_file = client.files.get(name=uploaded_file.name)

            state_name = getattr(uploaded_file.state, "name", str(uploaded_file.state))
            if state_name == "FAILED":
                print(f"\n[gemini/video-understanding] video processing failed: {getattr(uploaded_file, 'error', 'unknown')}", flush=True)
                contents = prompt
            else:
                print(f"\n[gemini/video-understanding] video active. Running virality detection on {GEMINI_MODEL}...", flush=True)
                contents = [uploaded_file, prompt]
        except Exception as e:
            print(f"\n[gemini/video-understanding] error uploading video ({e}), falling back to transcript prompt", flush=True)
            contents = prompt

    try:
        response = client.models.generate_content(
            model=GEMINI_MODEL,
            contents=contents,
            config={
                "temperature": 0.2,
                "response_mime_type": "application/json",
                "max_output_tokens": 8192,
            },
        )
        return response.text or ""
    finally:
        if uploaded_file is not None:
            try:
                client.files.delete(name=uploaded_file.name)
            except Exception:
                pass


def call_local_llm(prompt: str, video_path: Optional[str] = None) -> str:
    """Dispatch to the configured LLM provider (default: gemini)."""
    provider = (LLM_PROVIDER or "gemini").strip().lower()
    if provider == "gemini":
        return call_gemini_llm(prompt, video_path=video_path)
    if provider == "openai":
        return call_openai_llm(prompt)
    raise RuntimeError(
        f"Unknown LLM_PROVIDER={provider!r}. Use 'gemini' or 'openai'."
    )
