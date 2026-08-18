# 模型网关与 Provider

本目录是独立的模型服务运行单元，由固定 Model Gateway、OpenAI Adapter、CosyVoice 3 和 faster-whisper 组成。业务应用只访问 `http://127.0.0.1:8110` 的固定能力 API，不包含任何供应商请求逻辑。

第一次启动会自动从 `.env.example` 创建 `local-providers/.env`。需要先填写 OpenAI API Key，随后可以启用本地模型、选择 CPU/GPU 和调整端口：

```bash
make models
```

如果修改网关端口或把整套模型服务部署到其他机器，只需要在应用根 `.env` 中修改 `TKTKGO_MODEL_GATEWAY_URL`；内部 Adapter 地址和模型运行参数仍只存在于本目录配置。

## 固定模型网关

网关协议和扩展要求见 [`gateway/README.md`](gateway/README.md)。网关当前提供 OpenAI 的文案、图片、口播和字幕 Adapter，并把本地口播、字幕请求转发给对应执行服务。

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

完成配置后执行 `make models`。Model Gateway 会探测执行服务健康状态，业务 API 只代理统一能力目录；前端新建项目时会自动显示 CosyVoice 及其可用音色。
