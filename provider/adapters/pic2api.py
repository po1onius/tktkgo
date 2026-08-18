"""Pic2API gpt-image-2 图片 Adapter。"""

from __future__ import annotations

import logging
import re
from typing import Any

import httpx
from fastapi import HTTPException

from .common import BinaryResult, checked, image_prompt, json_object

logger = logging.getLogger("tktkgo.model_gateway.adapters.pic2api")

# Pic2API 把图片地址包装在 Markdown 图片语法中，而不是 OpenAI 的 data[].url。
IMAGE_PATTERN = re.compile(r"!\[[^\]]*\]\((?P<url>https?://[^\s)]+)\)")


class Pic2APIAdapter:
    provider_id = "pic2api"
    label = "Pic2API"
    # 当前供应商只提供这个固定模型，不允许业务请求选择未知模型。
    supported_image_model = "gpt-image-2"

    def __init__(self, *, base_url: str, api_key: str, image_models: list[str]) -> None:
        self.base_url = base_url.rstrip("/")
        self.api_key = api_key
        self.image_models = list(image_models)

    @property
    def available(self) -> bool:
        return bool(self.api_key)

    def _require_model(self, model: str) -> None:
        if not self.api_key:
            raise HTTPException(
                status_code=503, detail="Pic2API Adapter 未配置 API Key"
            )
        if model not in self.image_models:
            raise HTTPException(
                status_code=409,
                detail=f"Pic2API 不允许模型 {model}，可选模型: {self.image_models}",
            )

    def _headers(self, request_id: str) -> dict[str, str]:
        return {
            "Authorization": f"Bearer {self.api_key}",
            "X-Client-Request-Id": request_id,
            "Idempotency-Key": request_id,
        }

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
        self._require_model(model)
        # 目前已确认的能力只接受 1K 方图；最终画幅由网关媒体适配层统一裁剪。
        upstream_size = "1024x1024"
        logger.info(
            "调用 Pic2API 图片接口 request_id=%s model=%s upstream_size=%s target_width=%d "
            "target_height=%d ignored_quality=%s",
            request_id,
            model,
            upstream_size,
            width,
            height,
            quality,
        )
        response = await checked(
            await client.post(
                f"{self.base_url}/images/generations",
                headers=self._headers(request_id),
                json={
                    "model": model,
                    "prompt": image_prompt(prompt, width, height),
                    "size": upstream_size,
                },
            ),
            self.provider_id,
        )
        payload = json_object(response, "Pic2API")
        image_url = self.image_url(payload)
        image_response = await checked(
            await client.get(image_url, follow_redirects=True),
            "pic2api-image",
        )
        image = image_response.content
        logger.info(
            "Pic2API 图片下载完成 request_id=%s response_id=%s image_bytes=%d",
            request_id,
            payload.get("id"),
            len(image),
        )
        return BinaryResult(
            content=image,
            request_id=response.headers.get("x-request-id", request_id),
        )

    @staticmethod
    def image_url(payload: dict[str, Any]) -> str:
        """从 choices[].message.content 的 Markdown 图片语法中读取 URL。"""

        choices = payload.get("choices")
        if not isinstance(choices, list):
            raise HTTPException(status_code=502, detail="Pic2API 响应缺少 choices 数组")
        for choice in choices:
            if not isinstance(choice, dict):
                continue
            message = choice.get("message")
            content = message.get("content") if isinstance(message, dict) else None
            if not isinstance(content, str):
                continue
            match = IMAGE_PATTERN.search(content)
            if match:
                return match.group("url")
        logger.error(
            "Pic2API 响应中没有图片 URL response_id=%s model=%s choices=%d",
            payload.get("id"),
            payload.get("model"),
            len(choices),
        )
        raise HTTPException(status_code=502, detail="Pic2API 未返回 Markdown 图片 URL")
