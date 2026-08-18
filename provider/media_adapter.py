"""固定二进制媒体协议的归一化与校验。

供应商只能尽力返回接近目标的图片和音频；Model Gateway 在此完成最终裁剪、格式
归一化和解码校验，因此 Rust Pipeline 永远只接收精确尺寸 PNG 与有效 WAV。
"""

from __future__ import annotations

import io
import logging
import wave

from fastapi import HTTPException
from PIL import Image, ImageOps, UnidentifiedImageError

logger = logging.getLogger("tktkgo.model_gateway.media")


def crop_image(image: bytes, width: int, height: int) -> bytes:
    """按 EXIF 方向矫正后等比放大、居中裁剪，并输出精确尺寸 PNG。"""

    try:
        with Image.open(io.BytesIO(image)) as source:
            source.load()
            oriented = ImageOps.exif_transpose(source)
            source_size = oriented.size
            has_alpha = "A" in oriented.getbands() or "transparency" in oriented.info
            normalized = oriented.convert("RGBA" if has_alpha else "RGB")
            cropped = ImageOps.fit(
                normalized,
                (width, height),
                method=Image.Resampling.LANCZOS,
                centering=(0.5, 0.5),
            )
            output = io.BytesIO()
            cropped.save(output, format="PNG", optimize=True)
    except (UnidentifiedImageError, OSError, ValueError) as error:
        raise HTTPException(
            status_code=502, detail=f"上游返回的图片无法解码: {error}"
        ) from error
    result = output.getvalue()
    logger.info(
        "图片裁剪完成 source_width=%d source_height=%d output_width=%d output_height=%d output_bytes=%d",
        source_size[0],
        source_size[1],
        width,
        height,
        len(result),
    )
    return result


def validate_wav(audio: bytes) -> None:
    """验证适配层确实履行 WAV 输出协议，避免把上游 JSON 错误页伪装成音频。"""

    try:
        with wave.open(io.BytesIO(audio), "rb") as wav:
            channels = wav.getnchannels()
            sample_rate = wav.getframerate()
            frames = wav.getnframes()
    except (wave.Error, EOFError) as error:
        raise HTTPException(
            status_code=502, detail=f"上游返回的 WAV 无法解码: {error}"
        ) from error
    if channels <= 0 or sample_rate <= 0 or frames <= 0:
        raise HTTPException(status_code=502, detail="上游返回了空 WAV")
    logger.info(
        "WAV 校验完成 channels=%d sample_rate=%d frames=%d audio_bytes=%d",
        channels,
        sample_rate,
        frames,
        len(audio),
    )
