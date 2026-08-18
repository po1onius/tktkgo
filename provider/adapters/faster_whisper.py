"""faster-whisper 本地字幕执行服务 Adapter。"""

from __future__ import annotations

from typing import Any

import httpx

from .common import TranscriptionResult, checked, json_object, probe_health


class FasterWhisperAdapter:
    provider_id = "faster_whisper"
    label = "faster-whisper（本地）"

    def __init__(self, *, base_url: str) -> None:
        self.base_url = base_url.rstrip("/")

    async def health(self, client: httpx.AsyncClient) -> dict[str, Any] | None:
        return await probe_health(client, self.base_url, "faster-whisper")

    async def transcribe(
        self,
        client: httpx.AsyncClient,
        *,
        model: str,
        audio: bytes,
        canonical_text: str,
        request_id: str,
    ) -> TranscriptionResult:
        response = await checked(
            await client.post(
                f"{self.base_url}/v1/align",
                data={
                    "model": model,
                    "canonical_text": canonical_text,
                    "request_id": request_id,
                },
                files={"file": ("speech.wav", audio, "audio/wav")},
            ),
            self.provider_id,
        )
        payload = json_object(response, "faster-whisper")
        raw_cues = payload.get("cues")
        cues = (
            [cue for cue in raw_cues if isinstance(cue, dict)]
            if isinstance(raw_cues, list)
            else []
        )
        return TranscriptionResult(
            cues=cues,
            request_id=response.headers.get("x-request-id", request_id),
        )
