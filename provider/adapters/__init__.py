"""模型供应商 Adapter。

每个模块只封装对应供应商的鉴权、模型约束和 HTTP 请求/响应差异；固定网关协议、
Provider 路由以及最终媒体校验仍由 provider.py 统一负责。
"""

from .cosyvoice import CosyVoiceAdapter
from .deepseek import DeepSeekAdapter
from .openai import OpenAIAdapter
from .pic2api import Pic2APIAdapter
from .whisperx import WhisperXAdapter

__all__ = [
    "CosyVoiceAdapter",
    "DeepSeekAdapter",
    "OpenAIAdapter",
    "Pic2APIAdapter",
    "WhisperXAdapter",
]
