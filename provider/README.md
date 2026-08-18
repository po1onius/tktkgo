# 固定 Model Gateway API

业务服务只能依赖以下稳定端点，不允许调用供应商 API：

| 端点 | 输入 | 输出 |
|---|---|---|
| `GET /v1/models` | 无 | 四类能力及可用 Provider/模型目录 |
| `POST /v1/text/generate` | Provider、模型、提示词、JSON Schema | `{output, usage}` |
| `POST /v1/images/generate` | 提示词、宽、高、质量 | `image/png` 二进制 |
| `POST /v1/audio/speech` | 文本、音色、WAV 格式 | `audio/wav` 二进制 |
| `POST /v1/audio/transcribe` | WAV、语言上下文 | `{cues, usage}` |

每个生成请求都必须携带 `provider`、`model` 和 `request_id`。二进制响应通过 `X-Provider`、`X-Model`、`X-Request-Id`、`X-Latency-Ms` 返回元数据；失败使用非 2xx 状态和 JSON `detail`。

Pydantic 请求模型是网关协议的源定义；服务启动后可通过 `/openapi.json` 或 `/docs` 查看机器可读契约。Rust 侧所有调用集中在一个 `ModelGatewayClient`，不在各业务阶段重复实现 HTTP 协议。

`provider.py` 只负责固定路由、模型目录、Provider 分发与最终媒体校验。供应商实现位于 `adapters/`：每个 Adapter 独立封装鉴权、模型校验和上游响应差异，通过 `common.py` 的统一结果类型返回给网关。`config.py` 只读取本目录的 `providers.toml`，校验 Provider 凭据、转发地址和每类能力的模型列表后再注入 Adapter。

在本目录执行 `make` 可独立启动网关；根目录的 `make provider` 只是该命令的转发入口。网关不会安装、下载或拉起本地模型。CosyVoice 和 faster-whisper 只以普通 HTTP 上游存在，部署说明见 [`../local-models/README.md`](../local-models/README.md)。

图片 Adapter 会先选择供应商支持的最接近画幅，再由 [`media_adapter.py`](media_adapter.py) 等比缩放、居中裁剪成请求的精确宽高；口播响应也会在返回前完成 WAV 解码校验。Pic2API Adapter 固定使用 `gpt-image-2` 和已确认支持的 1024×1024 上游尺寸，从 `choices[].message.content` 的 Markdown 图片语法中提取 URL、下载图片后再进入同一媒体适配流程。

已支持 Provider 增加可选模型时只修改 `providers.toml`。新增 Provider 或新增能力时才需要实现 Adapter、扩展 `config.py` 的能力矩阵；若模型无法满足固定输出契约，Adapter 必须明确拒绝请求，不能把私有参数传回 Rust 业务服务。业务服务的稳定协议无需跟着供应商变化。
