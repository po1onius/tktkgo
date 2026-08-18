# 本地音频 Provider

本目录提供两个独立常驻 HTTP Provider：CosyVoice 3 生成 WAV 口播，faster-whisper 生成词级字幕时间戳。它们由 `make models` 单独管理，不依赖 PostgreSQL、Restate 或应用服务，并只读取本目录的 `.env`。

第一次启动会自动从 `.env.example` 创建 `local-providers/.env`。随后可以在该文件中启用模型、选择 CPU/GPU 和调整端口：

```bash
make models
```

如果修改了监听端口或把模型服务部署到其他机器，只需要在应用根 `.env` 中同步修改 `TKTKGO_COSYVOICE_BASE_URL` 或 `TKTKGO_FASTER_WHISPER_BASE_URL`；模型运行参数仍只存在于本目录配置。

## faster-whisper

依赖由 uv 锁定，`make models` 会自动同步并启动轻量 HTTP 服务；模型仅在前端首次选择并生成时加载。也可以在本目录手动运行：

```bash
cd local-providers/faster-whisper
uv sync --frozen
uv run python provider.py
```

CPU 默认使用 `large-v3 + int8`。资源不足时可在 `local-providers/.env` 中改成 `small`；NVIDIA CUDA 12 + cuDNN 9 环境建议使用 `cuda + float16`。

## CosyVoice 3

CosyVoice 当前没有可直接安装的 Python 包，并依赖官方仓库的子模块及其固定运行时版本。按官方兼容环境单独安装，不在业务代码中添加环境兼容逻辑：

```bash
cd local-providers/cosyvoice
git clone --recursive https://github.com/FunAudioLLM/CosyVoice.git
uv venv --python 3.10 .venv
uv pip install --python .venv/bin/python -r CosyVoice/requirements.txt
uv pip install --python .venv/bin/python fastapi==0.141.1 uvicorn==0.52.3
```

复制音色配置，并准备一段干净的单人参考 WAV：

```bash
cp voices.example.json voices.json
```

`prompt_text` 必须与参考 WAV 中说出的内容完全一致。配置多个音色时，JSON 键会作为前端可选的 `voice`。模型权重在第一次实际生成时由 CosyVoice 官方 ModelScope 下载；也可以将 `TKTKGO_COSYVOICE_MODEL` 指向已经下载的本地目录。

然后在 `local-providers/.env` 中设置：

```dotenv
TKTKGO_COSYVOICE_ENABLED=true
```

手动启动命令：

```bash
TKTKGO_COSYVOICE_ROOT="$PWD/CosyVoice" \
TKTKGO_COSYVOICE_VOICES_FILE="$PWD/voices.json" \
.venv/bin/python provider.py
```

完成配置后执行 `make models`。API 会通过 HTTP 探测服务健康状态，前端新建项目时会自动显示 CosyVoice 及其可用音色。
