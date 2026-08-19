"""OpenAI 兼容 Responses API 的结构化文本公共实现。"""

from __future__ import annotations

import json
from typing import Any

import httpx
from fastapi import HTTPException

from .common import TextResult, checked, json_object


async def generate_structured_text(
    client: httpx.AsyncClient,
    *,
    provider_id: str,
    provider_label: str,
    base_url: str,
    api_key: str,
    model: str,
    system: str,
    prompt: str,
    schema_name: str,
    output_schema: dict[str, Any],
    request_id: str,
) -> TextResult:
    """调用兼容 Responses API，并把 JSON Schema 输出归一为固定文本结果。"""

    response = await checked(
        await client.post(
            f"{base_url}/responses",
            headers={
                "Authorization": f"Bearer {api_key}",
                "X-Client-Request-Id": request_id,
                "Idempotency-Key": request_id,
            },
            json={
                "model": model,
                "input": [
                    {"role": "system", "content": system},
                    {"role": "user", "content": prompt},
                ],
                "text": {
                    "format": {
                        "type": "json_schema",
                        "name": schema_name,
                        "strict": True,
                        "schema": output_schema,
                    }
                },
                "reasoning": {"effort": "medium"},
            },
        ),
        provider_id,
    )
    payload = json_object(response, provider_label)
    text = output_text(payload)
    if text is None:
        raise HTTPException(
            status_code=502, detail=f"{provider_label} 未返回结构化文本"
        )
    try:
        output = json.loads(text)
    except json.JSONDecodeError as error:
        raise HTTPException(
            status_code=502,
            detail=f"{provider_label} 结构化文本不是合法 JSON: {error}",
        ) from error
    if not isinstance(output, dict):
        raise HTTPException(
            status_code=502, detail=f"{provider_label} 结构化文本不是 JSON 对象"
        )
    usage = payload.get("usage")
    usage = usage if isinstance(usage, dict) else {}
    return TextResult(
        output=output,
        request_id=response.headers.get("x-request-id", request_id),
        input_units=optional_int(usage.get("input_tokens")),
        output_units=optional_int(usage.get("output_tokens")),
    )


def output_text(payload: dict[str, Any]) -> str | None:
    # 部分 Responses API 实现会提供便捷的顶层 output_text，有值时优先使用。
    direct = payload.get("output_text")
    if isinstance(direct, str) and direct.strip():
        return direct
    outputs = payload.get("output")
    if not isinstance(outputs, list):
        return None
    for output in outputs:
        # DeepSeek 等推理模型会在 message 之前返回 reasoning item，其中也有
        # text 字段。只接受协议定义的 message/output_text，避免把推理过程
        # 误当作 JSON Schema 结果解析。
        if (
            not isinstance(output, dict)
            or output.get("type") != "message"
            or not isinstance(output.get("content"), list)
        ):
            continue
        for content in output["content"]:
            if (
                isinstance(content, dict)
                and content.get("type") == "output_text"
                and isinstance(content.get("text"), str)
                and content["text"].strip()
            ):
                return content["text"]
    return None


def optional_int(value: Any) -> int | None:
    return value if isinstance(value, int) and not isinstance(value, bool) else None
