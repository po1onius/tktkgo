"""WhisperX 本地强制对齐执行服务 Adapter。"""

from __future__ import annotations

from typing import Any

import httpx

from .common import AlignmentResult, checked, json_object, probe_health


class WhisperXAdapter:
    provider_id = "whisperx"
    label = "WhisperX 强制对齐（本地）"

    def __init__(self, *, base_url: str) -> None:
        self.base_url = base_url.rstrip("/")

    async def health(self, client: httpx.AsyncClient) -> dict[str, Any] | None:
        return await probe_health(client, self.base_url, "whisperx")

    async def align(
        self,
        client: httpx.AsyncClient,
        *,
        model: str,
        audio: bytes,
        canonical_text: str,
        request_id: str,
    ) -> AlignmentResult:
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
        payload = json_object(response, "WhisperX")
        raw_cues = payload.get("cues")
        cues = (
            [cue for cue in raw_cues if isinstance(cue, dict)]
            if isinstance(raw_cues, list)
            else []
        )
        return AlignmentResult(
            cues=cues,
            request_id=response.headers.get("x-request-id", request_id),
        )
