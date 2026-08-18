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
from fastapi import FastAPI, HTTPException, Response
from pydantic import BaseModel, ConfigDict, Field, TypeAdapter


def env(name: str, default: str | None = None) -> str:
    value = os.getenv(name, default or "").strip()
    if not value:
        raise RuntimeError(f"环境变量 {name} 不能为空")
    return value


COSYVOICE_ROOT = Path(
    env("TKTKGO_COSYVOICE_ROOT", "./local-providers/cosyvoice/CosyVoice")
).expanduser().resolve()
MODEL_NAME = env(
    "TKTKGO_COSYVOICE_MODEL", "FunAudioLLM/Fun-CosyVoice3-0.5B-2512"
)
VOICES_FILE = Path(
    env("TKTKGO_COSYVOICE_VOICES_FILE", "./local-providers/cosyvoice/voices.json")
).expanduser().resolve()
HOST = env("TKTKGO_COSYVOICE_HOST", "127.0.0.1")
PORT = int(env("TKTKGO_COSYVOICE_PORT", "8101"))
FP16 = env("TKTKGO_COSYVOICE_FP16", "false").lower() in {"1", "true", "yes"}

logging.basicConfig(
    level=os.getenv("TKTKGO_LOCAL_PROVIDER_LOG", "INFO").upper(),
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
    model: Any = None
    voices: dict[str, VoiceProfile] = {}
    load_lock = asyncio.Lock()
    lock = asyncio.Lock()


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
        "CosyVoice Provider 已启动，模型将在首次请求时加载 model=%s fp16=%s voices=%s",
        MODEL_NAME,
        FP16,
        sorted(state.voices),
    )
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
    """延迟加载大模型，使前端选择 OpenAI 时不占用本地 GPU/内存。"""

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
        # CosyVoice 3 的 instruct2 模板要求控制标记位于系统提示和具体指令之间。
        # voices.json 只保存自然语言指令，避免音色配置泄漏模型协议细节。
        instructions = f"You are a helpful assistant.<|endofprompt|>{instructions}"
        generated = model.inference_instruct2(
            request.input,
            instructions,
            profile.prompt_wav,
            stream=False,
        )
    else:
        generated = model.inference_zero_shot(
            request.input,
            profile.prompt_text,
            profile.prompt_wav,
            stream=False,
        )

    chunks = [
        item["tts_speech"].detach().cpu().squeeze().numpy()
        for item in generated
    ]
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

    uvicorn.run(app, host=HOST, port=PORT, log_level="info")
