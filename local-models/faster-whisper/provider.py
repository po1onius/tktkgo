"""基于 faster-whisper 的本地词级字幕 HTTP Provider。"""

from __future__ import annotations

import asyncio
import io
import logging
import time
from contextlib import asynccontextmanager
from typing import Annotated, Any

from config import load_config, resolve_config_path
from fastapi import FastAPI, File, Form, HTTPException, UploadFile
from faster_whisper import WhisperModel
from pydantic import BaseModel

settings, settings_path = load_config()
MODEL_NAME = settings.model.name
DEVICE = settings.model.device
COMPUTE_TYPE = settings.model.compute_type
LANGUAGE = settings.model.language
CPU_THREADS = settings.model.cpu_threads
NUM_WORKERS = settings.model.num_workers
DOWNLOAD_ROOT = resolve_config_path(settings_path, settings.model.download_root)

logging.basicConfig(
    level=settings.server.log_level,
    format="%(asctime)s %(levelname)s %(name)s %(message)s",
)
logger = logging.getLogger("tktkgo.faster_whisper")


class CaptionCue(BaseModel):
    text: str
    start_ms: int
    end_ms: int


class AlignmentResponse(BaseModel):
    cues: list[CaptionCue]
    language: str
    language_probability: float
    duration_seconds: float


class RuntimeState:
    model: WhisperModel | None = None
    load_lock = asyncio.Lock()
    # CTranslate2 可以配置 worker 并发；HTTP 层仍限流，避免单 GPU 被无限请求压垮。
    semaphore = asyncio.Semaphore(max(1, NUM_WORKERS))


state = RuntimeState()


@asynccontextmanager
async def lifespan(_: FastAPI):
    logger.info(
        "faster-whisper 开始准备模型 model=%s download_root=%s device=%s compute_type=%s cpu_threads=%d workers=%d",
        MODEL_NAME,
        DOWNLOAD_ROOT,
        DEVICE,
        COMPUTE_TYPE,
        CPU_THREADS,
        NUM_WORKERS,
    )
    # WhisperModel 会从 Hugging Face 自动下载缺失权重。启动阶段完成加载，健康检查
    # 返回成功时即可确定该独立部署已经具备实际处理请求的能力。
    await ensure_model()
    logger.info("faster-whisper Provider 已就绪 model=%s", MODEL_NAME)
    yield
    state.model = None
    logger.info("faster-whisper Provider 已停止")


app = FastAPI(title="tktkgo faster-whisper provider", lifespan=lifespan)


@app.get("/health")
async def health() -> dict[str, Any]:
    return {
        "status": "ok",
        "provider": "faster-whisper",
        "model": MODEL_NAME,
        "device": DEVICE,
        "compute_type": COMPUTE_TYPE,
        "loaded": state.model is not None,
    }


async def ensure_model() -> WhisperModel:
    """串行下载并加载模型，避免多个初始化请求重复占用磁盘和内存。"""

    if state.model is not None:
        return state.model
    async with state.load_lock:
        if state.model is not None:
            return state.model
        started = time.perf_counter()
        logger.info(
            "开始加载 faster-whisper model=%s device=%s compute_type=%s",
            MODEL_NAME,
            DEVICE,
            COMPUTE_TYPE,
        )
        state.model = await asyncio.to_thread(
            WhisperModel,
            MODEL_NAME,
            device=DEVICE,
            compute_type=COMPUTE_TYPE,
            cpu_threads=CPU_THREADS,
            num_workers=NUM_WORKERS,
            download_root=str(DOWNLOAD_ROOT),
        )
        logger.info(
            "faster-whisper 加载完成 model=%s elapsed_ms=%d",
            MODEL_NAME,
            round((time.perf_counter() - started) * 1000),
        )
        return state.model


def transcribe(audio: bytes, canonical_text: str) -> AlignmentResponse:
    model = state.model
    if model is None:
        raise RuntimeError("faster-whisper 模型尚未加载")

    segments, info = model.transcribe(
        io.BytesIO(audio),
        language=LANGUAGE,
        beam_size=5,
        word_timestamps=True,
        initial_prompt=canonical_text,
        vad_filter=False,
    )
    cues: list[CaptionCue] = []
    for segment in segments:
        for word in segment.words or []:
            text = word.word.strip()
            if text and word.end > word.start:
                cues.append(
                    CaptionCue(
                        text=text,
                        start_ms=round(word.start * 1000),
                        end_ms=round(word.end * 1000),
                    )
                )
    if not cues:
        raise RuntimeError("faster-whisper 未返回有效的词级时间戳")
    return AlignmentResponse(
        cues=cues,
        language=info.language,
        language_probability=info.language_probability,
        duration_seconds=info.duration,
    )


@app.post("/v1/align", response_model=AlignmentResponse)
async def align(
    file: Annotated[UploadFile, File()],
    model: Annotated[str, Form()],
    canonical_text: Annotated[str, Form()],
    request_id: Annotated[str, Form()],
) -> AlignmentResponse:
    if model != MODEL_NAME:
        raise HTTPException(
            status_code=409,
            detail=f"请求模型 {model} 与已加载模型 {MODEL_NAME} 不一致",
        )
    if not canonical_text.strip():
        raise HTTPException(status_code=422, detail="canonical_text 不能为空")
    audio = await file.read()
    if not audio:
        raise HTTPException(status_code=422, detail="上传音频不能为空")

    started = time.perf_counter()
    logger.info(
        "字幕对齐开始 request_id=%s model=%s audio_bytes=%d text_chars=%d",
        request_id,
        model,
        len(audio),
        len(canonical_text),
    )
    try:
        await ensure_model()
        async with state.semaphore:
            result = await asyncio.to_thread(transcribe, audio, canonical_text)
    except Exception as error:
        logger.exception("字幕对齐失败 request_id=%s model=%s", request_id, model)
        raise HTTPException(status_code=502, detail=str(error)) from error
    logger.info(
        "字幕对齐完成 request_id=%s model=%s cues=%d duration_seconds=%.3f elapsed_ms=%d",
        request_id,
        model,
        len(result.cues),
        result.duration_seconds,
        round((time.perf_counter() - started) * 1000),
    )
    return result


if __name__ == "__main__":
    import uvicorn

    uvicorn.run(
        app,
        host=settings.server.host,
        port=settings.server.port,
        log_level=settings.server.log_level.lower(),
    )
