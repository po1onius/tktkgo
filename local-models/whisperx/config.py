"""WhisperX 强制对齐服务的独立 TOML 配置。"""

from __future__ import annotations

import os
from pathlib import Path

import tomllib
from pydantic import BaseModel, ConfigDict, Field, ValidationError, field_validator


class StrictModel(BaseModel):
    model_config = ConfigDict(extra="forbid", hide_input_in_errors=True)


class ServerConfig(StrictModel):
    host: str = "127.0.0.1"
    port: int = Field(default=8102, ge=1, le=65535)
    log_level: str = "INFO"

    @field_validator("log_level")
    @classmethod
    def validate_log_level(cls, value: str) -> str:
        normalized = value.strip().upper()
        if normalized not in {"DEBUG", "INFO", "WARNING", "ERROR", "CRITICAL"}:
            raise ValueError(f"不支持的日志级别: {value}")
        return normalized


class ModelConfig(StrictModel):
    # 该模型是 WhisperX 官方中文默认对齐模型，模型卡使用 Apache-2.0 许可证。
    name: str = Field(min_length=1)
    download_root: str = "./models"
    device: str = "cpu"
    language: str = "zh"
    cpu_threads: int = Field(default=0, ge=0)
    num_workers: int = Field(default=1, ge=1)


class AppConfig(StrictModel):
    server: ServerConfig = Field(default_factory=ServerConfig)
    model: ModelConfig


def default_config_path() -> Path:
    configured = os.getenv("TKTKGO_WHISPERX_CONFIG", "").strip()
    if configured:
        return Path(configured).expanduser().resolve()
    return Path(__file__).resolve().parent / "provider.toml"


def load_config(path: Path | None = None) -> tuple[AppConfig, Path]:
    config_path = (path or default_config_path()).resolve()
    if not config_path.is_file():
        raise RuntimeError(f"WhisperX 配置文件不存在: {config_path}")
    try:
        with config_path.open("rb") as file:
            return AppConfig.model_validate(tomllib.load(file)), config_path
    except tomllib.TOMLDecodeError as error:
        raise RuntimeError(f"WhisperX TOML 语法错误: {error}") from error
    except ValidationError as error:
        raise RuntimeError(f"WhisperX 配置校验失败:\n{error}") from error


def resolve_config_path(config_path: Path, value: str) -> Path:
    """部署路径相对配置文件解析，避免从不同工作目录启动时指向不同缓存。"""

    path = Path(value).expanduser()
    return (
        path.resolve() if path.is_absolute() else (config_path.parent / path).resolve()
    )
