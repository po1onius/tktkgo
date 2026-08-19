# 本地模型独立部署

本目录只管理本地模型执行服务，不保存远程 Provider Key，也不启动固定 Provider 网关。两个模型目录都是独立的 uv 项目，分别拥有自己的 `.venv`、`uv.lock`、配置、模型缓存和进程生命周期，不通过 Makefile 安装或启动。

## faster-whisper

进入模型目录，首次创建本地配置并同步锁定环境：

```bash
cd local-models/faster-whisper
cp provider.example.toml provider.toml
uv sync --frozen
```

以后在该目录直接启动：

```bash
uv run --frozen python provider.py
```

`uv sync` 会在当前目录生成独立的 `.venv`。也可以在同步完成后使用 `.venv/bin/python provider.py` 启动。缺失权重会在服务启动时下载到本目录的 `models/`。默认使用 `large-v3 + CPU int8`；资源不足可以改用 `small`，NVIDIA CUDA 12 + cuDNN 9 环境建议使用 `cuda + float16`。服务在模型完成加载后才通过健康检查。

## CosyVoice 3

CosyVoice 3 尚无对应的稳定发布标签，因此先在模型目录获取官方仓库的已验证提交及其子模块：

```bash
cd local-models/cosyvoice
git clone https://github.com/FunAudioLLM/CosyVoice.git CosyVoice
git -C CosyVoice checkout 074ca6dc9e80a2f424f1f74b48bdd7d3fea531cc
git -C CosyVoice submodule update --init --recursive
```

准备独立 Python 3.10 环境和本地配置：

```bash
cp provider.example.toml provider.toml
cp voices.example.json voices.json
uv sync --frozen
```

请把干净的单人参考 WAV 放入 `voices/`，并修改 `voices.json`，确保 `prompt_text` 与录音内容完全一致。完成后在当前目录直接启动：

```bash
uv run --frozen python provider.py
```

也可以在同步完成后使用 `.venv/bin/python provider.py` 启动。`pyproject.toml` 固定官方提交验证过的 CPU 推理依赖，并只从 PyTorch 官方 CPU 索引安装 `torch` 和 `torchaudio`；因此默认不要求 NVIDIA 驱动、CUDA、TensorRT 或 `onnxruntime-gpu`。缺失的 ModelScope 权重会在服务启动时下载到当前目录的 `models/`。GPU 部署需要手动按 CosyVoice 官方要求调整依赖和系统驱动，不在 CPU 锁文件中混合两套运行时。

## 与网关连接

本地服务默认分别监听 `http://127.0.0.1:8102` 和 `http://127.0.0.1:8101`。在 `provider/providers.toml` 中配置相同 `base_url`，并将对应模型加入 `models.transcription.faster_whisper` 或 `models.speech.cosy_voice`。网关会探测健康接口，只有部署模型与目录模型一致时才向前端标记为可用。
