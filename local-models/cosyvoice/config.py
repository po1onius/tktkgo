"""CosyVoice 执行服务的独立 TOML 配置。"""

from __future__ import annotations

import os
from pathlib import Path

import tomli
from pydantic import BaseModel, ConfigDict, Field, ValidationError, field_validator


class StrictModel(BaseModel):
    model_config = ConfigDict(extra="forbid", hide_input_in_errors=True)


class ServerConfig(StrictModel):
    host: str = "127.0.0.1"
    port: int = Field(default=8101, ge=1, le=65535)
    log_level: str = "INFO"

    @field_validator("log_level")
    @classmethod
    def validate_log_level(cls, value: str) -> str:
        normalized = value.strip().upper()
        if normalized not in {"DEBUG", "INFO", "WARNING", "ERROR", "CRITICAL"}:
            raise ValueError(f"不支持的日志级别: {value}")
        return normalized


class ModelConfig(StrictModel):
    name: str = Field(min_length=1)
    cache_dir: str = "./models"
    fp16: bool = False


class RuntimeConfig(StrictModel):
    cosyvoice_root: str = "./CosyVoice"
    voices_file: str = "./voices.json"


class AppConfig(StrictModel):
    server: ServerConfig = Field(default_factory=ServerConfig)
    model: ModelConfig
    runtime: RuntimeConfig = Field(default_factory=RuntimeConfig)


def default_config_path() -> Path:
    configured = os.getenv("TKTKGO_COSYVOICE_CONFIG", "").strip()
    if configured:
        return Path(configured).expanduser().resolve()
    return Path(__file__).resolve().parent / "provider.toml"


def load_config(path: Path | None = None) -> tuple[AppConfig, Path]:
    config_path = (path or default_config_path()).resolve()
    if not config_path.is_file():
        raise RuntimeError(f"CosyVoice 配置文件不存在: {config_path}")
    try:
        with config_path.open("rb") as file:
            return AppConfig.model_validate(tomli.load(file)), config_path
    except tomli.TOMLDecodeError as error:
        raise RuntimeError(f"CosyVoice TOML 语法错误: {error}") from error
    except ValidationError as error:
        raise RuntimeError(f"CosyVoice 配置校验失败:\n{error}") from error


def resolve_config_path(config_path: Path, value: str) -> Path:
    path = Path(value).expanduser()
    return (
        path.resolve() if path.is_absolute() else (config_path.parent / path).resolve()
    )
