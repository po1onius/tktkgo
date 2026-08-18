"""供应商 Adapter 的共享结果类型与 HTTP 边界处理。"""

from __future__ import annotations

import logging
from dataclasses import dataclass
from typing import Any

import httpx
from fastapi import HTTPException

logger = logging.getLogger("tktkgo.model_gateway.adapters")


@dataclass(frozen=True)
class BinaryResult:
    """图片或音频 Adapter 的统一二进制结果。"""

    content: bytes
    request_id: str


@dataclass(frozen=True)
class TextResult:
    """结构化文本结果及供应商用量元数据。"""

    output: dict[str, Any]
    request_id: str
    input_units: int | None
    output_units: int | None


@dataclass(frozen=True)
class TranscriptionResult:
    """词级字幕结果；具体字段随后仍由网关 Pydantic 协议校验。"""

    cues: list[dict[str, Any]]
    request_id: str


async def checked(response: httpx.Response, provider: str) -> httpx.Response:
    """把所有供应商非成功 HTTP 响应统一映射为网关 502。"""

    if response.is_success:
        return response
    body = response.text[:4000]
    logger.error(
        "上游模型请求失败 provider=%s status=%d body=%s",
        provider,
        response.status_code,
        body,
    )
    raise HTTPException(
        status_code=502,
        detail=f"{provider} HTTP {response.status_code}: {body}",
    )


def json_object(response: httpx.Response, provider: str) -> dict[str, Any]:
    """读取供应商 JSON 对象，避免非法 JSON 变成没有上下文的内部错误。"""

    try:
        payload = response.json()
    except ValueError as error:
        raise HTTPException(
            status_code=502, detail=f"{provider} 返回了非法 JSON"
        ) from error
    if not isinstance(payload, dict):
        raise HTTPException(status_code=502, detail=f"{provider} 返回的 JSON 不是对象")
    return payload


def image_prompt(prompt: str, width: int, height: int) -> str:
    """提示图片供应商为网关的最终裁剪保留中央安全区。"""

    return (
        f"{prompt}\n最终目标画幅为 {width}:{height}。请把主要主体和关键视觉信息放在中央安全区域，"
        "画面边缘允许在保持构图的前提下被裁剪。"
    )


async def probe_health(
    client: httpx.AsyncClient,
    base_url: str,
    expected_provider: str,
) -> dict[str, Any] | None:
    """探测本地执行服务；不可用时从目录隐藏，但不影响模型网关自身启动。"""

    try:
        response = await client.get(f"{base_url}/health", timeout=2)
        response.raise_for_status()
        result = response.json()
        if not isinstance(result, dict):
            raise TypeError("健康响应不是 JSON 对象")
        if result.get("status") != "ok" or result.get("provider") != expected_provider:
            raise RuntimeError(f"健康响应不匹配: {result}")
        return result
    except (httpx.HTTPError, TypeError, ValueError, RuntimeError) as error:
        logger.warning("本地模型不可用 provider=%s error=%s", expected_provider, error)
        return None
