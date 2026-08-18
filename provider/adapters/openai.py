"""OpenAI 文本、图片、口播和字幕 Adapter。"""

from __future__ import annotations

import base64
import binascii
import logging
from typing import Any, ClassVar

import httpx
from fastapi import HTTPException

from .common import (
    BinaryResult,
    TextResult,
    TranscriptionResult,
    checked,
    image_prompt,
    json_object,
)
from .responses import generate_structured_text

logger = logging.getLogger("tktkgo.model_gateway.adapters.openai")


class OpenAIAdapter:
    provider_id = "openai"
    label = "OpenAI"
    voices: ClassVar[tuple[str, ...]] = ("coral", "alloy", "sage")

    def __init__(
        self,
        *,
        base_url: str,
        api_key: str,
        text_models: list[str],
        image_models: list[str],
        speech_models: list[str],
        transcription_models: list[str],
    ) -> None:
        self.base_url = base_url.rstrip("/")
        self.api_key = api_key
        self.text_models = list(text_models)
        self.image_models = list(image_models)
        self.speech_models = list(speech_models)
        self.transcription_models = list(transcription_models)

    @property
    def available(self) -> bool:
        return bool(self.api_key)

    def _require_model(
        self, model: str, allowed_models: list[str], capability: str
    ) -> None:
        if not self.api_key:
            raise HTTPException(status_code=503, detail="OpenAI Adapter 未配置 API Key")
        if model not in allowed_models:
            raise HTTPException(
                status_code=409,
                detail=f"OpenAI {capability} 不允许模型 {model}，可选模型: {allowed_models}",
            )

    def _headers(self, request_id: str) -> dict[str, str]:
        return {
            "Authorization": f"Bearer {self.api_key}",
            "X-Client-Request-Id": request_id,
            # 工作流的稳定任务 ID 直接传给供应商，便于日志关联并支持上游幂等处理。
            "Idempotency-Key": request_id,
        }

    async def generate_text(
        self,
        client: httpx.AsyncClient,
        *,
        model: str,
        system: str,
        prompt: str,
        schema_name: str,
        output_schema: dict[str, Any],
        request_id: str,
    ) -> TextResult:
        self._require_model(model, self.text_models, "文案")
        return await generate_structured_text(
            client,
            provider_id=self.provider_id,
            provider_label=self.label,
            base_url=self.base_url,
            api_key=self.api_key,
            model=model,
            system=system,
            prompt=prompt,
            schema_name=schema_name,
            output_schema=output_schema,
            request_id=request_id,
        )

    async def generate_image(
        self,
        client: httpx.AsyncClient,
        *,
        model: str,
        prompt: str,
        width: int,
        height: int,
        quality: str,
        request_id: str,
    ) -> BinaryResult:
        self._require_model(model, self.image_models, "图片")
        response = await checked(
            await client.post(
                f"{self.base_url}/images/generations",
                headers=self._headers(request_id),
                json={
                    "model": model,
                    "prompt": image_prompt(prompt, width, height),
                    "size": self._image_size(width, height),
                    "quality": quality,
                    "output_format": "png",
                },
            ),
            self.provider_id,
        )
        payload = json_object(response, "OpenAI")
        data = payload.get("data")
        item = (
            data[0]
            if isinstance(data, list) and data and isinstance(data[0], dict)
            else {}
        )
        if item.get("b64_json"):
            try:
                image = base64.b64decode(item["b64_json"], validate=True)
            except (binascii.Error, ValueError) as error:
                raise HTTPException(
                    status_code=502, detail="OpenAI 返回了非法 Base64 图片"
                ) from error
        elif item.get("url"):
            image_response = await checked(
                await client.get(str(item["url"]), follow_redirects=True),
                "openai-image",
            )
            image = image_response.content
        else:
            raise HTTPException(status_code=502, detail="OpenAI 图片接口未返回图片")
        logger.info(
            "OpenAI 图片获取完成 request_id=%s image_bytes=%d", request_id, len(image)
        )
        return BinaryResult(
            content=image,
            request_id=response.headers.get("x-request-id", request_id),
        )

    async def generate_speech(
        self,
        client: httpx.AsyncClient,
        *,
        model: str,
        text: str,
        voice: str,
        output_format: str,
        instructions: str | None,
        request_id: str,
    ) -> BinaryResult:
        self._require_model(model, self.speech_models, "口播")
        response = await checked(
            await client.post(
                f"{self.base_url}/audio/speech",
                headers=self._headers(request_id),
                json={
                    "model": model,
                    "voice": voice,
                    "input": text,
                    "response_format": output_format,
                    "instructions": instructions,
                },
            ),
            self.provider_id,
        )
        return BinaryResult(
            content=response.content,
            request_id=response.headers.get("x-request-id", request_id),
        )

    async def transcribe(
        self,
        client: httpx.AsyncClient,
        *,
        model: str,
        audio: bytes,
        canonical_text: str,
        request_id: str,
    ) -> TranscriptionResult:
        self._require_model(model, self.transcription_models, "字幕")
        response = await checked(
            await client.post(
                f"{self.base_url}/audio/transcriptions",
                headers=self._headers(request_id),
                data={
                    "model": model,
                    "response_format": "verbose_json",
                    "timestamp_granularities[]": "word",
                    "prompt": canonical_text,
                },
                files={"file": ("speech.wav", audio, "audio/wav")},
            ),
            self.provider_id,
        )
        payload = json_object(response, "OpenAI")
        words = payload.get("words")
        cues: list[dict[str, Any]] = []
        if isinstance(words, list):
            for word in words:
                if not isinstance(word, dict):
                    continue
                text = str(word.get("word", "")).strip()
                start = self._optional_float(word.get("start"))
                end = self._optional_float(word.get("end"))
                if text and start is not None and end is not None and end > start:
                    cues.append(
                        {
                            "text": text,
                            "start_ms": round(start * 1000),
                            "end_ms": round(end * 1000),
                        }
                    )
        return TranscriptionResult(
            cues=cues,
            request_id=response.headers.get("x-request-id", request_id),
        )

    @staticmethod
    def _image_size(width: int, height: int) -> str:
        if width == height:
            return "1024x1024"
        return "1024x1536" if height > width else "1536x1024"

    @staticmethod
    def _optional_float(value: Any) -> float | None:
        try:
            return float(value)
        except (TypeError, ValueError):
            return None
