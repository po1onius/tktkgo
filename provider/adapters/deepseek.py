"""DeepSeek Responses API 结构化文案 Adapter。"""

from __future__ import annotations

from typing import Any

import httpx
from fastapi import HTTPException

from .common import TextResult
from .responses import generate_structured_text


class DeepSeekAdapter:
    provider_id = "deepseek"
    label = "DeepSeek"

    def __init__(self, *, base_url: str, api_key: str, text_models: list[str]) -> None:
        self.base_url = base_url.rstrip("/")
        self.api_key = api_key
        self.text_models = list(text_models)

    @property
    def available(self) -> bool:
        return bool(self.api_key)

    def _require_model(self, model: str) -> None:
        if not self.api_key:
            raise HTTPException(
                status_code=503, detail="DeepSeek Adapter 未配置 API Key"
            )
        if model not in self.text_models:
            raise HTTPException(
                status_code=409,
                detail=f"DeepSeek 文案不允许模型 {model}，可选模型: {self.text_models}",
            )

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
        self._require_model(model)
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
