"""CosyVoice 本地口播执行服务 Adapter。"""

from __future__ import annotations

from typing import Any

import httpx

from .common import BinaryResult, checked, probe_health


class CosyVoiceAdapter:
    provider_id = "cosy_voice"
    label = "CosyVoice 3（本地）"

    def __init__(self, *, base_url: str) -> None:
        self.base_url = base_url.rstrip("/")

    async def health(self, client: httpx.AsyncClient) -> dict[str, Any] | None:
        return await probe_health(client, self.base_url, "cosyvoice")

    async def generate_speech(
        self,
        client: httpx.AsyncClient,
        *,
        model: str,
        text: str,
        voice: str,
        instructions: str | None,
        request_id: str,
    ) -> BinaryResult:
        response = await checked(
            await client.post(
                f"{self.base_url}/v1/speech",
                json={
                    "model": model,
                    "voice": voice,
                    "input": text,
                    "instructions": instructions,
                    "request_id": request_id,
                },
            ),
            self.provider_id,
        )
        return BinaryResult(
            content=response.content,
            request_id=response.headers.get("x-request-id", request_id),
        )
