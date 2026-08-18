"""基于 Fun-CosyVoice 3 的本地中文口播 HTTP Provider。"""

from __future__ import annotations

import asyncio
import io
import logging
import os
import sys
import time
import wave
from contextlib import asynccontextmanager
from pathlib import Path
from typing import Any

import numpy as np
from config import load_config, resolve_config_path
from fastapi import FastAPI, HTTPException, Response
from pydantic import BaseModel, ConfigDict, Field, TypeAdapter

settings, settings_path = load_config()
COSYVOICE_ROOT = resolve_config_path(settings_path, settings.runtime.cosyvoice_root)
MODEL_NAME = settings.model.name
MODEL_CACHE = resolve_config_path(settings_path, settings.model.cache_dir)
VOICES_FILE = resolve_config_path(settings_path, settings.runtime.voices_file)
FP16 = settings.model.fp16

# CosyVoice 官方加载器通过 ModelScope 下载远程模型。缓存目录由本部署独占，
# 避免本地模型文件与 Provider 网关或用户全局缓存形成隐式耦合。
MODEL_CACHE.mkdir(parents=True, exist_ok=True)
os.environ["MODELSCOPE_CACHE"] = str(MODEL_CACHE)

logging.basicConfig(
    level=settings.server.log_level,
    format="%(asctime)s %(levelname)s %(name)s %(message)s",
)
logger = logging.getLogger("tktkgo.cosyvoice")


class VoiceProfile(BaseModel):
    model_config = ConfigDict(extra="forbid")

    prompt_text: str = Field(min_length=1)
    prompt_wav: str = Field(min_length=1)
    instruction: str | None = None


class SpeechRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    model: str = Field(min_length=1)
    voice: str = Field(min_length=1)
    input: str = Field(min_length=1, max_length=20_000)
    instructions: str | None = None
    request_id: str = Field(min_length=1)


class RuntimeState:
    def __init__(self) -> None:
        self.model: Any = None
        self.voices: dict[str, VoiceProfile] = {}
        self.load_lock = asyncio.Lock()
        self.lock = asyncio.Lock()


state = RuntimeState()


def load_voices() -> dict[str, VoiceProfile]:
    if not VOICES_FILE.is_file():
        raise RuntimeError(f"音色配置不存在: {VOICES_FILE}")
    voices = TypeAdapter(dict[str, VoiceProfile]).validate_json(
        VOICES_FILE.read_text(encoding="utf-8")
    )
    if not voices:
        raise RuntimeError("音色配置至少需要一个音色")
    for name, profile in voices.items():
        prompt_wav = Path(profile.prompt_wav).expanduser()
        if not prompt_wav.is_absolute():
            prompt_wav = (VOICES_FILE.parent / prompt_wav).resolve()
        if not prompt_wav.is_file():
            raise RuntimeError(f"音色 {name} 的参考音频不存在: {prompt_wav}")
        profile.prompt_wav = str(prompt_wav)
    return voices


def load_model() -> Any:
    if not COSYVOICE_ROOT.is_dir():
        raise RuntimeError(f"CosyVoice 官方仓库目录不存在: {COSYVOICE_ROOT}")
    sys.path.insert(0, str(COSYVOICE_ROOT))
    sys.path.insert(0, str(COSYVOICE_ROOT / "third_party" / "Matcha-TTS"))
    from cosyvoice.cli.cosyvoice import AutoModel

    return AutoModel(model_dir=MODEL_NAME, fp16=FP16)


@asynccontextmanager
async def lifespan(_: FastAPI):
    if not COSYVOICE_ROOT.is_dir():
        raise RuntimeError(f"CosyVoice 官方仓库目录不存在: {COSYVOICE_ROOT}")
    state.voices = load_voices()
    logger.info(
        "CosyVoice 开始准备模型 model=%s cache_dir=%s fp16=%s voices=%s",
        MODEL_NAME,
        MODEL_CACHE,
        FP16,
        sorted(state.voices),
    )
    # AutoModel 会自动下载缺失的 ModelScope 权重。把加载放在启动阶段，避免服务
    # 健康但第一次业务请求才暴露网络、磁盘或模型配置错误。
    await ensure_model()
    logger.info("CosyVoice Provider 已就绪 model=%s", MODEL_NAME)
    yield
    state.model = None
    logger.info("CosyVoice Provider 已停止")


app = FastAPI(title="tktkgo CosyVoice provider", lifespan=lifespan)


@app.get("/health")
async def health() -> dict[str, Any]:
    return {
        "status": "ok",
        "provider": "cosyvoice",
        "model": MODEL_NAME,
        "voices": sorted(state.voices),
        "loaded": state.model is not None,
    }


async def ensure_model() -> Any:
    """串行下载并加载模型，避免多个初始化请求争用同一缓存。"""

    if state.model is not None:
        return state.model
    async with state.load_lock:
        if state.model is not None:
            return state.model
        started = time.perf_counter()
        logger.info("开始加载 CosyVoice model=%s fp16=%s", MODEL_NAME, FP16)
        state.model = await asyncio.to_thread(load_model)
        logger.info(
            "CosyVoice 加载完成 model=%s sample_rate=%s elapsed_ms=%d",
            MODEL_NAME,
            state.model.sample_rate,
            round((time.perf_counter() - started) * 1000),
        )
        return state.model


def to_wav(chunks: list[np.ndarray], sample_rate: int) -> bytes:
    if not chunks:
        raise RuntimeError("CosyVoice 未返回任何音频片段")
    samples = np.concatenate(chunks)
    pcm = (np.clip(samples, -1.0, 1.0) * 32767.0).astype(np.int16)
    output = io.BytesIO()
    with wave.open(output, "wb") as wav:
        wav.setnchannels(1)
        wav.setsampwidth(2)
        wav.setframerate(sample_rate)
        wav.writeframes(pcm.tobytes())
    return output.getvalue()


def synthesize(request: SpeechRequest, profile: VoiceProfile) -> bytes:
    model = state.model
    if model is None:
        raise RuntimeError("CosyVoice 模型尚未加载")

    instructions = " ".join(
        value.strip()
        for value in (profile.instruction, request.instructions)
        if value and value.strip()
    )
    if instructions:
        # CosyVoice 3 官方 instruct2 协议要求 <|endofprompt|> 位于完整指令末尾。
        # voices.json 只保存自然语言指令，避免音色配置泄漏模型协议细节。
        instructions = f"You are a helpful assistant. {instructions}<|endofprompt|>"
        generated = model.inference_instruct2(
            request.input,
            instructions,
            profile.prompt_wav,
            stream=False,
        )
    else:
        # CosyVoice 3 的零样本模式会把参考文本和待合成文本拼接后送入 LLM，
        # 官方协议要求参考文本带有系统提示和结束标记。该标记不能依赖文本
        # 前端自动补齐，否则 wetext/ttsfrd 未安装时真实推理才会失败。
        prompt_text = (
            "You are a helpful assistant.<|endofprompt|>"
            f"{profile.prompt_text}"
        )
        generated = model.inference_zero_shot(
            request.input,
            prompt_text,
            profile.prompt_wav,
            stream=False,
        )

    chunks = [item["tts_speech"].detach().cpu().squeeze().numpy() for item in generated]
    return to_wav(chunks, model.sample_rate)


@app.post("/v1/speech")
async def speech(request: SpeechRequest) -> Response:
    if request.model != MODEL_NAME:
        raise HTTPException(
            status_code=409,
            detail=f"请求模型 {request.model} 与已加载模型 {MODEL_NAME} 不一致",
        )
    profile = state.voices.get(request.voice)
    if profile is None:
        raise HTTPException(
            status_code=422,
            detail=f"未知音色 {request.voice}，可用音色: {sorted(state.voices)}",
        )

    started = time.perf_counter()
    logger.info(
        "口播生成开始 request_id=%s model=%s voice=%s text_chars=%d",
        request.request_id,
        request.model,
        request.voice,
        len(request.input),
    )
    try:
        await ensure_model()
        # 单模型串行推理，避免并发请求争用同一组 GPU/模型状态。
        async with state.lock:
            audio = await asyncio.to_thread(synthesize, request, profile)
    except Exception as error:
        logger.exception(
            "口播生成失败 request_id=%s model=%s voice=%s",
            request.request_id,
            request.model,
            request.voice,
        )
        raise HTTPException(status_code=502, detail=str(error)) from error

    logger.info(
        "口播生成完成 request_id=%s model=%s voice=%s audio_bytes=%d elapsed_ms=%d",
        request.request_id,
        request.model,
        request.voice,
        len(audio),
        round((time.perf_counter() - started) * 1000),
    )
    return Response(
        content=audio,
        media_type="audio/wav",
        headers={"X-Request-Id": request.request_id},
    )


if __name__ == "__main__":
    import uvicorn

    uvicorn.run(
        app,
        host=settings.server.host,
        port=settings.server.port,
        log_level=settings.server.log_level.lower(),
    )
