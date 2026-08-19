"""模型服务 TOML 配置加载与跨 Provider 校验。"""

from __future__ import annotations

import os
from pathlib import Path
from typing import Literal

import tomllib
from pydantic import (
    BaseModel,
    ConfigDict,
    Field,
    ValidationError,
    field_validator,
    model_validator,
)

Capability = Literal["text", "image", "speech", "alignment"]

# 每种供应商只允许出现在确实实现的能力中，避免配置成功但运行时请求到不存在的端点。
SUPPORTED_PROVIDERS: dict[Capability, set[str]] = {
    "text": {"openai", "deepseek"},
    "image": {"openai", "pic2api"},
    "speech": {"openai", "cosy_voice"},
    "alignment": {"whisperx"},
}
REMOTE_PROVIDERS = {"openai", "deepseek", "pic2api"}


class StrictModel(BaseModel):
    # 配置错误中隐藏原始输入，确保 API Key 不会出现在启动日志里。
    model_config = ConfigDict(extra="forbid", hide_input_in_errors=True)


class GatewayConfig(StrictModel):
    host: str = "127.0.0.1"
    port: int = Field(default=8110, ge=1, le=65535)
    log_level: str = "INFO"

    @field_validator("log_level")
    @classmethod
    def validate_log_level(cls, value: str) -> str:
        normalized = value.strip().upper()
        if normalized not in {"DEBUG", "INFO", "WARNING", "ERROR", "CRITICAL"}:
            raise ValueError(f"不支持的日志级别: {value}")
        return normalized


class RemoteProviderConfig(StrictModel):
    api_key: str = ""
    base_url: str

    @field_validator("api_key", "base_url")
    @classmethod
    def strip_value(cls, value: str) -> str:
        return value.strip()

    @field_validator("base_url")
    @classmethod
    def validate_base_url(cls, value: str) -> str:
        if not value.startswith(("http://", "https://")):
            raise ValueError("base_url 必须以 http:// 或 https:// 开头")
        return value.rstrip("/")


class LocalProviderConfig(StrictModel):
    """网关只保存执行服务地址，不关心本地模型如何安装、下载或启动。"""

    base_url: str

    @field_validator("base_url")
    @classmethod
    def validate_base_url(cls, value: str) -> str:
        normalized = value.strip().rstrip("/")
        if not normalized.startswith(("http://", "https://")):
            raise ValueError("base_url 必须以 http:// 或 https:// 开头")
        return normalized


class ProvidersConfig(StrictModel):
    openai: RemoteProviderConfig
    deepseek: RemoteProviderConfig
    pic2api: RemoteProviderConfig
    whisperx: LocalProviderConfig
    cosy_voice: LocalProviderConfig


class ModelsConfig(StrictModel):
    text: dict[str, list[str]]
    image: dict[str, list[str]]
    speech: dict[str, list[str]]
    alignment: dict[str, list[str]]

    def for_capability(self, capability: Capability) -> dict[str, list[str]]:
        return getattr(self, capability)


class AppConfig(StrictModel):
    gateway: GatewayConfig = Field(default_factory=GatewayConfig)
    providers: ProvidersConfig
    models: ModelsConfig

    @model_validator(mode="after")
    def validate_provider_models(self) -> AppConfig:
        selected_providers: set[str] = set()
        for capability, supported in SUPPORTED_PROVIDERS.items():
            selections = self.models.for_capability(capability)
            non_empty = 0
            for provider, models in selections.items():
                if provider not in supported:
                    raise ValueError(f"{capability} 不支持 Provider {provider}")
                normalized = [model.strip() for model in models]
                if any(not model for model in normalized):
                    raise ValueError(f"{capability}.{provider} 包含空模型名")
                if len(normalized) != len(set(normalized)):
                    raise ValueError(f"{capability}.{provider} 包含重复模型")
                selections[provider] = normalized
                if normalized:
                    selected_providers.add(provider)
                    non_empty += len(normalized)
            if non_empty == 0:
                raise ValueError(f"models.{capability} 至少需要配置一个可选模型")

        for provider in sorted(selected_providers & REMOTE_PROVIDERS):
            config = getattr(self.providers, provider)
            if not config.api_key.strip():
                raise ValueError(
                    f"已选择 Provider {provider} 的模型，但没有配置 api_key"
                )

        # 每个本地执行服务进程只装载一个模型。网关与部署已经解耦，但同一个
        # base_url 仍不能同时声明多个执行模型，否则目录中会永久出现不可用项。
        local_capabilities: dict[str, Capability] = {
            "whisperx": "alignment",
            "cosy_voice": "speech",
        }
        for provider, capability in local_capabilities.items():
            models = self.models.for_capability(capability).get(provider, [])
            if len(models) > 1:
                raise ValueError(f"本地 Provider {provider} 最多只能配置一个模型")

        pic2api_models = self.models.image.get("pic2api", [])
        if any(model != "gpt-image-2" for model in pic2api_models):
            raise ValueError("Pic2API 当前只允许配置模型 gpt-image-2")
        return self

    def models_for(self, capability: Capability, provider: str) -> list[str]:
        return list(self.models.for_capability(capability).get(provider, []))


def default_config_path() -> Path:
    configured = os.getenv("TKTKGO_PROVIDERS_CONFIG", "").strip()
    if configured:
        return Path(configured).expanduser().resolve()
    return Path(__file__).resolve().parent / "providers.toml"


def load_config(path: Path | None = None) -> AppConfig:
    config_path = (path or default_config_path()).resolve()
    if not config_path.is_file():
        raise RuntimeError(f"模型配置文件不存在: {config_path}")
    try:
        with config_path.open("rb") as file:
            payload = tomllib.load(file)
        return AppConfig.model_validate(payload)
    except tomllib.TOMLDecodeError as error:
        raise RuntimeError(f"模型配置 TOML 语法错误: {error}") from error
    except ValidationError as error:
        raise RuntimeError(f"模型配置校验失败:\n{error}") from error
