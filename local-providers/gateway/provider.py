"""tktkgo 固定模型能力 API。

业务服务只依赖本文件定义的四类协议；OpenAI 和本地模型的参数、鉴权及响应差异
全部封装在网关内部，禁止泄漏到 Rust Pipeline。
"""

from __future__ import annotations

import asyncio
import base64
import binascii
import json
import logging
import os
import time
from contextlib import asynccontextmanager
from typing import Annotated, Any, Literal

import httpx
from fastapi import FastAPI, File, Form, HTTPException, Request, Response, UploadFile
from pydantic import BaseModel, ConfigDict, Field

from media_adapter import crop_image, validate_wav


def env(name: str, default: str = "", *, required: bool = False) -> str:
    value = os.getenv(name, default).strip()
    if required and not value:
        raise RuntimeError(f"环境变量 {name} 不能为空")
    return value


OPENAI_BASE_URL = env("TKTKGO_OPENAI_BASE_URL", "https://api.openai.com/v1").rstrip("/")
OPENAI_API_KEY = env("TKTKGO_OPENAI_API_KEY")
TEXT_MODEL = env("TKTKGO_TEXT_MODEL", "gpt-5.6-terra")
IMAGE_MODEL = env("TKTKGO_IMAGE_MODEL", "gpt-image-2")
TTS_MODEL = env("TKTKGO_TTS_MODEL", "gpt-4o-mini-tts")
TRANSCRIBE_MODEL = env("TKTKGO_TRANSCRIBE_MODEL", "whisper-1")
COSYVOICE_URL = env("TKTKGO_COSYVOICE_INTERNAL_URL", "http://127.0.0.1:8101").rstrip("/")
FASTER_WHISPER_URL = env(
    "TKTKGO_FASTER_WHISPER_INTERNAL_URL", "http://127.0.0.1:8102"
).rstrip("/")
HOST = env("TKTKGO_MODEL_GATEWAY_HOST", "127.0.0.1")
PORT = int(env("TKTKGO_MODEL_GATEWAY_PORT", "8110"))

logging.basicConfig(
    level=env("TKTKGO_MODEL_PROVIDER_LOG", "INFO").upper(),
    format="%(asctime)s %(levelname)s %(name)s %(message)s",
)
logger = logging.getLogger("tktkgo.model_gateway")


class ProviderOption(BaseModel):
    id: str
    label: str
    model: str
    available: bool
    voices: list[str] = Field(default_factory=list)


class ModelCatalog(BaseModel):
    text: list[ProviderOption]
    image: list[ProviderOption]
    speech: list[ProviderOption]
    transcription: list[ProviderOption]


class TextRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    provider: str
    model: str
    system: str
    prompt: str
    schema_name: str
    output_schema: dict[str, Any]
    request_id: str


class ImageRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    provider: str
    model: str
    prompt: str = Field(min_length=1, max_length=50_000)
    width: int = Field(ge=256, le=4096)
    height: int = Field(ge=256, le=4096)
    quality: Literal["low", "medium", "high"] = "high"
    request_id: str


class SpeechRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    provider: str
    model: str
    text: str = Field(min_length=1, max_length=20_000)
    voice: str = Field(min_length=1, max_length=128)
    output_format: Literal["wav"] = "wav"
    instructions: str | None = None
    request_id: str


class CaptionCue(BaseModel):
    text: str
    start_ms: int
    end_ms: int


class RuntimeState:
    client: httpx.AsyncClient | None = None


state = RuntimeState()


@asynccontextmanager
async def lifespan(_: FastAPI):
    state.client = httpx.AsyncClient(timeout=httpx.Timeout(30 * 60, connect=10))
    logger.info(
        "模型网关已启动 text_model=%s image_model=%s tts_model=%s transcription_model=%s openai_configured=%s",
        TEXT_MODEL,
        IMAGE_MODEL,
        TTS_MODEL,
        TRANSCRIBE_MODEL,
        bool(OPENAI_API_KEY),
    )
    yield
    await state.client.aclose()
    state.client = None
    logger.info("模型网关已停止")


app = FastAPI(title="tktkgo model gateway", lifespan=lifespan)


@app.middleware("http")
async def request_logging(request: Request, call_next: Any) -> Response:
    started = time.perf_counter()
    try:
        response = await call_next(request)
    except Exception:
        logger.exception(
            "模型网关请求异常 method=%s path=%s elapsed_ms=%d",
            request.method,
            request.url.path,
            round((time.perf_counter() - started) * 1000),
        )
        raise
    logger.info(
        "模型网关请求完成 method=%s path=%s status=%d elapsed_ms=%d",
        request.method,
        request.url.path,
        response.status_code,
        round((time.perf_counter() - started) * 1000),
    )
    return response


def client() -> httpx.AsyncClient:
    if state.client is None:
        raise RuntimeError("模型网关 HTTP Client 尚未初始化")
    return state.client


@app.get("/health")
async def health() -> dict[str, str]:
    return {"status": "ok", "provider": "model-gateway"}


async def local_health(url: str, expected_provider: str) -> dict[str, Any] | None:
    try:
        response = await client().get(f"{url}/health", timeout=2)
        response.raise_for_status()
        result = response.json()
        if result.get("status") != "ok" or result.get("provider") != expected_provider:
            raise RuntimeError(f"健康响应不匹配: {result}")
        return result
    except Exception as error:
        logger.warning("本地模型不可用 provider=%s error=%s", expected_provider, error)
        return None


@app.get("/v1/models", response_model=ModelCatalog)
async def models() -> ModelCatalog:
    cosyvoice, faster_whisper = await asyncio.gather(
        local_health(COSYVOICE_URL, "cosyvoice"),
        local_health(FASTER_WHISPER_URL, "faster-whisper"),
    )
    openai_available = bool(OPENAI_API_KEY)
    logger.info(
        "模型目录刷新完成 openai_available=%s cosyvoice_available=%s faster_whisper_available=%s",
        openai_available,
        bool(cosyvoice),
        bool(faster_whisper),
    )
    return ModelCatalog(
        text=[ProviderOption(id="openai", label="OpenAI", model=TEXT_MODEL, available=openai_available)],
        image=[ProviderOption(id="openai", label="OpenAI", model=IMAGE_MODEL, available=openai_available)],
        speech=[
            ProviderOption(
                id="openai",
                label="OpenAI",
                model=TTS_MODEL,
                available=openai_available,
                voices=["coral", "alloy", "sage"],
            ),
            ProviderOption(
                id="cosyvoice",
                label="CosyVoice 3（本地）",
                model=str(cosyvoice.get("model", "")) if cosyvoice else "",
                available=bool(cosyvoice and cosyvoice.get("model") and cosyvoice.get("voices")),
                voices=list(cosyvoice.get("voices", [])) if cosyvoice else [],
            ),
        ],
        transcription=[
            ProviderOption(
                id="openai",
                label="OpenAI",
                model=TRANSCRIBE_MODEL,
                available=openai_available,
            ),
            ProviderOption(
                id="faster-whisper",
                label="faster-whisper（本地）",
                model=str(faster_whisper.get("model", "")) if faster_whisper else "",
                available=bool(faster_whisper and faster_whisper.get("model")),
            ),
        ],
    )


def require_openai(provider: str, model: str, expected_model: str) -> None:
    if provider != "openai":
        raise HTTPException(status_code=422, detail=f"该能力不支持 Provider: {provider}")
    if not OPENAI_API_KEY:
        raise HTTPException(status_code=503, detail="OpenAI Adapter 未配置 API Key")
    if model != expected_model:
        raise HTTPException(
            status_code=409,
            detail=f"请求模型 {model} 与 Adapter 模型 {expected_model} 不一致",
        )


def openai_headers(request_id: str) -> dict[str, str]:
    return {
        "Authorization": f"Bearer {OPENAI_API_KEY}",
        "X-Client-Request-Id": request_id,
        # generation_tasks 的稳定 UUID 直接作为上游幂等键，Restate 重放不会重复计费生成。
        "Idempotency-Key": request_id,
    }


async def checked(response: httpx.Response, provider: str) -> httpx.Response:
    if response.is_success:
        return response
    logger.error(
        "上游模型请求失败 provider=%s status=%d body=%s",
        provider,
        response.status_code,
        response.text[:4000],
    )
    raise HTTPException(
        status_code=502,
        detail=f"{provider} HTTP {response.status_code}: {response.text[:4000]}",
    )


def output_text(payload: dict[str, Any]) -> str | None:
    direct = payload.get("output_text")
    if isinstance(direct, str):
        return direct
    for output in payload.get("output", []):
        for content in output.get("content", []):
            text = content.get("text")
            if isinstance(text, str):
                return text
    return None


@app.post("/v1/text/generate")
async def generate_text(request: TextRequest) -> dict[str, Any]:
    require_openai(request.provider, request.model, TEXT_MODEL)
    logger.info(
        "开始生成结构化文本 request_id=%s provider=%s model=%s schema=%s prompt_chars=%d",
        request.request_id,
        request.provider,
        request.model,
        request.schema_name,
        len(request.prompt),
    )
    started = time.perf_counter()
    response = await checked(
        await client().post(
            f"{OPENAI_BASE_URL}/responses",
            headers=openai_headers(request.request_id),
            json={
                "model": request.model,
                "input": [
                    {"role": "system", "content": request.system},
                    {"role": "user", "content": request.prompt},
                ],
                "text": {
                    "format": {
                        "type": "json_schema",
                        "name": request.schema_name,
                        "strict": True,
                        "schema": request.output_schema,
                    }
                },
                "reasoning": {"effort": "medium"},
            },
        ),
        "openai",
    )
    payload = response.json()
    text = output_text(payload)
    if text is None:
        raise HTTPException(status_code=502, detail="OpenAI 未返回结构化文本")
    try:
        output = json.loads(text)
    except json.JSONDecodeError as error:
        raise HTTPException(status_code=502, detail=f"结构化文本不是合法 JSON: {error}") from error
    return {
        "output": output,
        "usage": {
            "request_id": response.headers.get("x-request-id"),
            "input_units": payload.get("usage", {}).get("input_tokens"),
            "output_units": payload.get("usage", {}).get("output_tokens"),
            "latency_ms": round((time.perf_counter() - started) * 1000),
        },
    }


def openai_image_size(width: int, height: int) -> str:
    if width == height:
        return "1024x1024"
    return "1024x1536" if height > width else "1536x1024"


def openai_image_prompt(prompt: str, width: int, height: int) -> str:
    """提示供应商为最终裁剪保留安全区；精确尺寸仍由媒体适配层负责。"""

    return (
        f"{prompt}\n最终目标画幅为 {width}:{height}。请把主要主体和关键视觉信息放在中央安全区域，"
        "画面边缘允许在保持构图的前提下被裁剪。"
    )


@app.post("/v1/images/generate")
async def generate_image(request: ImageRequest) -> Response:
    require_openai(request.provider, request.model, IMAGE_MODEL)
    logger.info(
        "开始生成图片 request_id=%s provider=%s model=%s width=%d height=%d quality=%s prompt_chars=%d",
        request.request_id,
        request.provider,
        request.model,
        request.width,
        request.height,
        request.quality,
        len(request.prompt),
    )
    started = time.perf_counter()
    response = await checked(
        await client().post(
            f"{OPENAI_BASE_URL}/images/generations",
            headers=openai_headers(request.request_id),
            json={
                "model": request.model,
                "prompt": openai_image_prompt(request.prompt, request.width, request.height),
                "size": openai_image_size(request.width, request.height),
                "quality": request.quality,
                "output_format": "png",
            },
        ),
        "openai",
    )
    payload = response.json()
    item = (payload.get("data") or [{}])[0]
    if item.get("b64_json"):
        try:
            image = base64.b64decode(item["b64_json"], validate=True)
        except (binascii.Error, ValueError) as error:
            raise HTTPException(status_code=502, detail="OpenAI 返回了非法 Base64 图片") from error
    elif item.get("url"):
        image_response = await checked(await client().get(item["url"]), "openai-image")
        image = image_response.content
    else:
        raise HTTPException(status_code=502, detail="OpenAI 图片接口未返回图片")
    image = await asyncio.to_thread(crop_image, image, request.width, request.height)
    return Response(
        content=image,
        media_type="image/png",
        headers={
            "X-Provider": request.provider,
            "X-Model": request.model,
            "X-Request-Id": response.headers.get("x-request-id", request.request_id),
            "X-Latency-Ms": str(round((time.perf_counter() - started) * 1000)),
        },
    )


@app.post("/v1/audio/speech")
async def generate_speech(request: SpeechRequest) -> Response:
    started = time.perf_counter()
    logger.info(
        "开始生成口播 request_id=%s provider=%s model=%s voice=%s text_chars=%d",
        request.request_id,
        request.provider,
        request.model,
        request.voice,
        len(request.text),
    )
    if request.provider == "openai":
        require_openai(request.provider, request.model, TTS_MODEL)
        response = await checked(
            await client().post(
                f"{OPENAI_BASE_URL}/audio/speech",
                headers=openai_headers(request.request_id),
                json={
                    "model": request.model,
                    "voice": request.voice,
                    "input": request.text,
                    "response_format": request.output_format,
                    "instructions": request.instructions,
                },
            ),
            "openai",
        )
    elif request.provider == "cosyvoice":
        response = await checked(
            await client().post(
                f"{COSYVOICE_URL}/v1/speech",
                json={
                    "model": request.model,
                    "voice": request.voice,
                    "input": request.text,
                    "instructions": request.instructions,
                    "request_id": request.request_id,
                },
            ),
            "cosyvoice",
        )
    else:
        raise HTTPException(status_code=422, detail=f"不支持的口播 Provider: {request.provider}")
    audio = response.content
    await asyncio.to_thread(validate_wav, audio)
    return Response(
        content=audio,
        media_type="audio/wav",
        headers={
            "X-Provider": request.provider,
            "X-Model": request.model,
            "X-Request-Id": response.headers.get("x-request-id", request.request_id),
            "X-Latency-Ms": str(round((time.perf_counter() - started) * 1000)),
        },
    )


@app.post("/v1/audio/transcribe")
async def transcribe(
    file: Annotated[UploadFile, File()],
    provider: Annotated[str, Form()],
    model: Annotated[str, Form()],
    canonical_text: Annotated[str, Form()],
    request_id: Annotated[str, Form()],
) -> dict[str, Any]:
    audio = await file.read()
    if not audio:
        raise HTTPException(status_code=422, detail="上传音频不能为空")
    logger.info(
        "开始生成字幕 request_id=%s provider=%s model=%s audio_bytes=%d canonical_chars=%d",
        request_id,
        provider,
        model,
        len(audio),
        len(canonical_text),
    )
    started = time.perf_counter()
    files = {"file": ("speech.wav", audio, "audio/wav")}
    if provider == "openai":
        require_openai(provider, model, TRANSCRIBE_MODEL)
        response = await checked(
            await client().post(
                f"{OPENAI_BASE_URL}/audio/transcriptions",
                headers=openai_headers(request_id),
                data={
                    "model": model,
                    "response_format": "verbose_json",
                    "timestamp_granularities[]": "word",
                    "prompt": canonical_text,
                },
                files=files,
            ),
            "openai",
        )
        payload = response.json()
        cues = [
            CaptionCue(
                text=str(word.get("word", "")).strip(),
                start_ms=round(float(word.get("start", 0)) * 1000),
                end_ms=round(float(word.get("end", 0)) * 1000),
            )
            for word in payload.get("words", [])
            if str(word.get("word", "")).strip()
            and float(word.get("end", 0)) > float(word.get("start", 0))
        ]
    elif provider == "faster-whisper":
        response = await checked(
            await client().post(
                f"{FASTER_WHISPER_URL}/v1/align",
                data={
                    "model": model,
                    "canonical_text": canonical_text,
                    "request_id": request_id,
                },
                files=files,
            ),
            "faster-whisper",
        )
        payload = response.json()
        cues = [CaptionCue.model_validate(cue) for cue in payload.get("cues", [])]
    else:
        raise HTTPException(status_code=422, detail=f"不支持的字幕 Provider: {provider}")
    if not cues:
        raise HTTPException(status_code=502, detail=f"{provider} 未返回词级时间戳")
    return {
        "cues": [cue.model_dump() for cue in cues],
        "usage": {
            "request_id": response.headers.get("x-request-id", request_id),
            "latency_ms": round((time.perf_counter() - started) * 1000),
        },
    }


if __name__ == "__main__":
    import uvicorn

    uvicorn.run(app, host=HOST, port=PORT, log_level="info")
