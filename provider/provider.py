"""tktkgo 固定模型能力 API。

业务服务只依赖本文件定义的四类协议；各供应商的参数、鉴权及响应差异由独立
Adapter 封装在网关内部，禁止泄漏到 Rust Pipeline。
"""

from __future__ import annotations

import asyncio
import logging
import time
from contextlib import asynccontextmanager
from typing import Annotated, Any, Literal

import httpx
from adapters import (
    CosyVoiceAdapter,
    DeepSeekAdapter,
    OpenAIAdapter,
    Pic2APIAdapter,
    WhisperXAdapter,
)
from config import load_config
from fastapi import FastAPI, File, Form, HTTPException, Request, Response, UploadFile
from media_adapter import crop_image, validate_wav
from pydantic import BaseModel, ConfigDict, Field

settings = load_config()

logging.basicConfig(
    level=settings.gateway.log_level.upper(),
    format="%(asctime)s %(levelname)s %(name)s %(message)s",
)
logger = logging.getLogger("tktkgo.model_gateway")

# 网关持有 Adapter 实例及其静态配置；Adapter 自身不读取环境变量，配置来源保持单一。
openai_adapter = OpenAIAdapter(
    base_url=settings.providers.openai.base_url,
    api_key=settings.providers.openai.api_key,
    text_models=settings.models_for("text", "openai"),
    image_models=settings.models_for("image", "openai"),
    speech_models=settings.models_for("speech", "openai"),
)
deepseek_adapter = DeepSeekAdapter(
    base_url=settings.providers.deepseek.base_url,
    api_key=settings.providers.deepseek.api_key,
    text_models=settings.models_for("text", "deepseek"),
)
pic2api_adapter = Pic2APIAdapter(
    base_url=settings.providers.pic2api.base_url,
    api_key=settings.providers.pic2api.api_key,
    image_models=settings.models_for("image", "pic2api"),
)
cosyvoice_adapter = CosyVoiceAdapter(base_url=settings.providers.cosy_voice.base_url)
whisperx_adapter = WhisperXAdapter(base_url=settings.providers.whisperx.base_url)


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
    alignment: list[ProviderOption]


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
        "模型网关已启动 text_models=%s image_models=%s speech_models=%s "
        "alignment_models=%s openai_configured=%s deepseek_configured=%s "
        "pic2api_configured=%s",
        settings.models.text,
        settings.models.image,
        settings.models.speech,
        settings.models.alignment,
        openai_adapter.available,
        deepseek_adapter.available,
        pic2api_adapter.available,
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


@app.get("/v1/models", response_model=ModelCatalog)
async def models() -> ModelCatalog:
    cosyvoice, whisperx = await asyncio.gather(
        cosyvoice_adapter.health(client())
        if settings.models_for("speech", "cosy_voice")
        else asyncio.sleep(0, result=None),
        whisperx_adapter.health(client())
        if settings.models_for("alignment", "whisperx")
        else asyncio.sleep(0, result=None),
    )
    logger.info(
        "模型目录刷新完成 openai_available=%s deepseek_available=%s pic2api_available=%s "
        "cosyvoice_available=%s whisperx_available=%s",
        openai_adapter.available,
        deepseek_adapter.available,
        pic2api_adapter.available,
        bool(cosyvoice),
        bool(whisperx),
    )
    return ModelCatalog(
        text=[
            ProviderOption(
                id=openai_adapter.provider_id,
                label=openai_adapter.label,
                model=model,
                available=openai_adapter.available,
            )
            for model in openai_adapter.text_models
        ]
        + [
            ProviderOption(
                id=deepseek_adapter.provider_id,
                label=deepseek_adapter.label,
                model=model,
                available=deepseek_adapter.available,
            )
            for model in deepseek_adapter.text_models
        ],
        image=[
            ProviderOption(
                id=openai_adapter.provider_id,
                label=openai_adapter.label,
                model=model,
                available=openai_adapter.available,
            )
            for model in openai_adapter.image_models
        ]
        + [
            ProviderOption(
                id=pic2api_adapter.provider_id,
                label=pic2api_adapter.label,
                model=model,
                available=pic2api_adapter.available,
            )
            for model in pic2api_adapter.image_models
        ],
        speech=[
            ProviderOption(
                id=openai_adapter.provider_id,
                label=openai_adapter.label,
                model=model,
                available=openai_adapter.available,
                voices=openai_adapter.voices,
            )
            for model in openai_adapter.speech_models
        ]
        + [
            ProviderOption(
                id=cosyvoice_adapter.provider_id,
                label=cosyvoice_adapter.label,
                model=model,
                available=bool(
                    cosyvoice
                    and cosyvoice.get("model") == model
                    and cosyvoice.get("voices")
                ),
                voices=list(cosyvoice.get("voices", [])) if cosyvoice else [],
            )
            for model in settings.models_for("speech", "cosy_voice")
        ],
        alignment=[
            ProviderOption(
                id=whisperx_adapter.provider_id,
                label=whisperx_adapter.label,
                model=model,
                available=bool(whisperx and whisperx.get("model") == model),
            )
            for model in settings.models_for("alignment", "whisperx")
        ],
    )


@app.post("/v1/text/generate")
async def generate_text(request: TextRequest) -> dict[str, Any]:
    logger.info(
        "开始生成结构化文本 request_id=%s provider=%s model=%s schema=%s prompt_chars=%d",
        request.request_id,
        request.provider,
        request.model,
        request.schema_name,
        len(request.prompt),
    )
    started = time.perf_counter()
    if request.provider == openai_adapter.provider_id:
        adapter = openai_adapter
    elif request.provider == deepseek_adapter.provider_id:
        adapter = deepseek_adapter
    else:
        raise HTTPException(
            status_code=422, detail=f"不支持的文案 Provider: {request.provider}"
        )
    result = await adapter.generate_text(
        client(),
        model=request.model,
        system=request.system,
        prompt=request.prompt,
        schema_name=request.schema_name,
        output_schema=request.output_schema,
        request_id=request.request_id,
    )
    return {
        "output": result.output,
        "usage": {
            "request_id": result.request_id,
            "input_units": result.input_units,
            "output_units": result.output_units,
            "latency_ms": round((time.perf_counter() - started) * 1000),
        },
    }


@app.post("/v1/images/generate")
async def generate_image(request: ImageRequest) -> Response:
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
    if request.provider == openai_adapter.provider_id:
        result = await openai_adapter.generate_image(
            client(),
            model=request.model,
            prompt=request.prompt,
            width=request.width,
            height=request.height,
            quality=request.quality,
            request_id=request.request_id,
        )
    elif request.provider == pic2api_adapter.provider_id:
        result = await pic2api_adapter.generate_image(
            client(),
            model=request.model,
            prompt=request.prompt,
            width=request.width,
            height=request.height,
            quality=request.quality,
            request_id=request.request_id,
        )
    else:
        raise HTTPException(
            status_code=422, detail=f"不支持的图片 Provider: {request.provider}"
        )
    image = await asyncio.to_thread(
        crop_image, result.content, request.width, request.height
    )
    return Response(
        content=image,
        media_type="image/png",
        headers={
            "X-Provider": request.provider,
            "X-Model": request.model,
            "X-Request-Id": result.request_id,
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
    if request.provider == openai_adapter.provider_id:
        result = await openai_adapter.generate_speech(
            client(),
            model=request.model,
            text=request.text,
            voice=request.voice,
            output_format=request.output_format,
            instructions=request.instructions,
            request_id=request.request_id,
        )
    elif request.provider == cosyvoice_adapter.provider_id:
        result = await cosyvoice_adapter.generate_speech(
            client(),
            model=request.model,
            text=request.text,
            voice=request.voice,
            instructions=request.instructions,
            request_id=request.request_id,
        )
    else:
        raise HTTPException(
            status_code=422, detail=f"不支持的口播 Provider: {request.provider}"
        )
    await asyncio.to_thread(validate_wav, result.content)
    return Response(
        content=result.content,
        media_type="audio/wav",
        headers={
            "X-Provider": request.provider,
            "X-Model": request.model,
            "X-Request-Id": result.request_id,
            "X-Latency-Ms": str(round((time.perf_counter() - started) * 1000)),
        },
    )


@app.post("/v1/audio/align")
async def align_audio(
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
        "开始强制对齐字幕 request_id=%s provider=%s model=%s audio_bytes=%d canonical_chars=%d",
        request_id,
        provider,
        model,
        len(audio),
        len(canonical_text),
    )
    started = time.perf_counter()
    if provider == whisperx_adapter.provider_id:
        result = await whisperx_adapter.align(
            client(),
            model=model,
            audio=audio,
            canonical_text=canonical_text,
            request_id=request_id,
        )
    else:
        raise HTTPException(
            status_code=422, detail=f"不支持的强制对齐 Provider: {provider}"
        )
    cues = [CaptionCue.model_validate(cue) for cue in result.cues]
    if not cues:
        raise HTTPException(status_code=502, detail=f"{provider} 未返回字符级时间戳")
    return {
        "cues": [cue.model_dump() for cue in cues],
        "usage": {
            "request_id": result.request_id,
            "latency_ms": round((time.perf_counter() - started) * 1000),
        },
    }


if __name__ == "__main__":
    import uvicorn

    uvicorn.run(
        app,
        host=settings.gateway.host,
        port=settings.gateway.port,
        log_level=settings.gateway.log_level.lower(),
    )
