"""基于 WhisperX CTC 的本地中文强制对齐 HTTP Provider。

服务只把调用方提供的确定文稿对齐到音频，不执行 Whisper 转写。这样字幕文本永远
来自已经审核的口播原文，模型只负责计算字符出现的时间，彻底隔离 ASR 幻觉。
"""

from __future__ import annotations

import asyncio
import logging
import math
import tempfile
import time
from contextlib import asynccontextmanager
from pathlib import Path
from typing import Annotated, Any

import nltk
import torch
import whisperx
from fastapi import FastAPI, File, Form, HTTPException, Response, UploadFile
from pydantic import BaseModel

from config import load_config, resolve_config_path

settings, settings_path = load_config()
MODEL_NAME = settings.model.name
DEVICE = settings.model.device
LANGUAGE = settings.model.language
CPU_THREADS = settings.model.cpu_threads
NUM_WORKERS = settings.model.num_workers
DOWNLOAD_ROOT = resolve_config_path(settings_path, settings.model.download_root)

logging.basicConfig(
    level=settings.server.log_level,
    format="%(asctime)s %(levelname)s %(name)s %(message)s",
)
logger = logging.getLogger("tktkgo.whisperx")


class CaptionCue(BaseModel):
    text: str
    start_ms: int
    end_ms: int


class AlignmentResponse(BaseModel):
    cues: list[CaptionCue]
    language: str
    duration_seconds: float


class RuntimeState:
    model: Any | None = None
    metadata: dict[str, Any] | None = None
    load_lock = asyncio.Lock()
    # 对齐模型会占用大量内存；HTTP 层显式限流，避免同一设备被并发推理打满。
    semaphore = asyncio.Semaphore(max(1, NUM_WORKERS))


state = RuntimeState()


@asynccontextmanager
async def lifespan(_: FastAPI):
    logger.info(
        "WhisperX 开始准备强制对齐模型 model=%s language=%s download_root=%s device=%s cpu_threads=%d workers=%d",
        MODEL_NAME,
        LANGUAGE,
        DOWNLOAD_ROOT,
        DEVICE,
        CPU_THREADS,
        NUM_WORKERS,
    )
    await ensure_model()
    logger.info("WhisperX 强制对齐 Provider 已就绪 model=%s", MODEL_NAME)
    yield
    state.model = None
    state.metadata = None
    logger.info("WhisperX 强制对齐 Provider 已停止")


app = FastAPI(title="tktkgo WhisperX forced-alignment provider", lifespan=lifespan)


@app.get("/health")
async def health() -> dict[str, Any]:
    return {
        "status": "ok",
        "provider": "whisperx",
        "model": MODEL_NAME,
        "language": LANGUAGE,
        "device": DEVICE,
        "loaded": state.model is not None and state.metadata is not None,
    }


async def ensure_model() -> tuple[Any, dict[str, Any]]:
    """串行下载并加载模型，健康检查成功即表示服务具备真实对齐能力。"""

    if state.model is not None and state.metadata is not None:
        return state.model, state.metadata
    async with state.load_lock:
        if state.model is not None and state.metadata is not None:
            return state.model, state.metadata
        if CPU_THREADS > 0:
            torch.set_num_threads(CPU_THREADS)
        DOWNLOAD_ROOT.mkdir(parents=True, exist_ok=True)
        await asyncio.to_thread(ensure_nltk_data)
        started = time.perf_counter()
        logger.info(
            "开始加载 WhisperX 强制对齐模型 model=%s language=%s device=%s",
            MODEL_NAME,
            LANGUAGE,
            DEVICE,
        )
        state.model, state.metadata = await asyncio.to_thread(
            whisperx.load_align_model,
            language_code=LANGUAGE,
            device=DEVICE,
            model_name=MODEL_NAME,
            model_dir=str(DOWNLOAD_ROOT),
        )
        logger.info(
            "WhisperX 强制对齐模型加载完成 model=%s elapsed_ms=%d",
            MODEL_NAME,
            round((time.perf_counter() - started) * 1000),
        )
        return state.model, state.metadata


def ensure_nltk_data() -> None:
    """在健康检查前准备 WhisperX 分句器，避免首个对齐请求才触发隐式下载。"""

    nltk_root = DOWNLOAD_ROOT / "nltk_data"
    nltk_root.mkdir(parents=True, exist_ok=True)
    nltk_root_text = str(nltk_root)
    if nltk_root_text not in nltk.data.path:
        nltk.data.path.insert(0, nltk_root_text)
    try:
        nltk.data.find("tokenizers/punkt_tab/english", paths=[nltk_root_text])
        logger.info("NLTK punkt_tab 分句数据已就绪 path=%s", nltk_root)
    except LookupError:
        logger.info("开始下载 NLTK punkt_tab 分句数据 path=%s", nltk_root)
        if not nltk.download("punkt_tab", download_dir=nltk_root_text, quiet=True):
            raise RuntimeError(
                "无法下载 WhisperX 必需的 NLTK punkt_tab；请检查网络后手动执行 "
                f"python -m nltk.downloader -d {nltk_root} punkt_tab"
            )
        nltk.data.find("tokenizers/punkt_tab/english", paths=[nltk_root_text])
        logger.info("NLTK punkt_tab 分句数据下载完成 path=%s", nltk_root)


def load_wav(audio: bytes) -> Any:
    """交给 WhisperX/FFmpeg 统一解码、降混并重采样到对齐模型要求的 16kHz。"""

    temporary_path: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(suffix=".wav", delete=False) as temporary:
            temporary.write(audio)
            temporary_path = Path(temporary.name)
        return whisperx.load_audio(str(temporary_path))
    finally:
        if temporary_path is not None:
            temporary_path.unlink(missing_ok=True)


def cues_from_alignment(result: dict[str, Any], canonical_text: str) -> list[CaptionCue]:
    """把 WhisperX 字符结果转换为严格保持原文顺序的时间单元。

    标点和空白没有独立声学帧时合并到前一个已对齐字符；开头的非发音字符则合并到
    第一个字符。这里绝不采用模型识别文本，也不允许静默丢字。
    """

    aligned_characters: list[dict[str, Any]] = []
    for segment in result.get("segments", []):
        if isinstance(segment, dict):
            chars = segment.get("chars")
            if isinstance(chars, list):
                aligned_characters.extend(item for item in chars if isinstance(item, dict))

    reconstructed = "".join(str(item.get("char", "")) for item in aligned_characters)
    if reconstructed != canonical_text:
        raise RuntimeError(
            "WhisperX 返回的字符序列与确定原文不一致，拒绝生成可能错字的字幕"
        )

    cues: list[CaptionCue] = []
    leading_unaligned = ""
    for item in aligned_characters:
        character = str(item.get("char", ""))
        start = item.get("start")
        end = item.get("end")
        has_timestamp = (
            isinstance(start, (int, float))
            and isinstance(end, (int, float))
            and math.isfinite(float(start))
            and math.isfinite(float(end))
            and float(end) > float(start)
        )
        if not has_timestamp:
            if cues:
                cues[-1].text += character
            else:
                leading_unaligned += character
            continue

        start_ms = round(float(start) * 1000)
        end_ms = round(float(end) * 1000)
        if cues:
            start_ms = max(start_ms, cues[-1].end_ms)
        if end_ms <= start_ms:
            # 极短音素在毫秒取整后可能与前一字符落在同一点；合并仍可保持完整原文，
            # 并避免向下游制造倒序或零时长时间戳。
            if cues:
                cues[-1].text += character
            else:
                leading_unaligned += character
            continue
        cues.append(
            CaptionCue(
                text=f"{leading_unaligned}{character}",
                start_ms=start_ms,
                end_ms=end_ms,
            )
        )
        leading_unaligned = ""

    if leading_unaligned:
        if not cues:
            raise RuntimeError("WhisperX 没有返回任何可用的字符时间戳")
        cues[-1].text += leading_unaligned
    if not cues:
        raise RuntimeError("WhisperX 没有返回任何可用的字符时间戳")
    if "".join(cue.text for cue in cues) != canonical_text:
        raise RuntimeError("强制对齐结果未完整覆盖确定原文")
    return cues


def force_align(audio: bytes, canonical_text: str) -> AlignmentResponse:
    model = state.model
    metadata = state.metadata
    if model is None or metadata is None:
        raise RuntimeError("WhisperX 强制对齐模型尚未加载")

    waveform = load_wav(audio)
    duration_seconds = len(waveform) / 16_000
    if duration_seconds <= 0:
        raise RuntimeError("上传音频没有可对齐的采样")
    result = whisperx.align(
        [{"start": 0.0, "end": duration_seconds, "text": canonical_text}],
        model,
        metadata,
        waveform,
        DEVICE,
        return_char_alignments=True,
        print_progress=False,
    )
    return AlignmentResponse(
        cues=cues_from_alignment(result, canonical_text),
        language=LANGUAGE,
        duration_seconds=duration_seconds,
    )


@app.post("/v1/align", response_model=AlignmentResponse)
async def align(
    response: Response,
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
    text = canonical_text.strip()
    if not text:
        raise HTTPException(status_code=422, detail="canonical_text 不能为空")
    audio = await file.read()
    if not audio:
        raise HTTPException(status_code=422, detail="上传音频不能为空")

    started = time.perf_counter()
    logger.info(
        "强制对齐开始 request_id=%s model=%s audio_bytes=%d text_chars=%d",
        request_id,
        model,
        len(audio),
        len(text),
    )
    try:
        await ensure_model()
        async with state.semaphore:
            result = await asyncio.to_thread(force_align, audio, text)
    except Exception as error:
        logger.exception("强制对齐失败 request_id=%s model=%s", request_id, model)
        raise HTTPException(status_code=502, detail=str(error)) from error
    elapsed_ms = round((time.perf_counter() - started) * 1000)
    response.headers["X-Request-Id"] = request_id
    logger.info(
        "强制对齐完成 request_id=%s model=%s cues=%d duration_seconds=%.3f elapsed_ms=%d",
        request_id,
        model,
        len(result.cues),
        result.duration_seconds,
        elapsed_ms,
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
