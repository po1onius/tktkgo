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

图片 Adapter 会先选择供应商支持的最接近画幅，再由 [`media_adapter.py`](media_adapter.py) 等比缩放、居中裁剪成请求的精确宽高；口播响应也会在返回前完成 WAV 解码校验。

新增模型时只修改本目录的供应商 Adapter 和 `/v1/models` 目录项。若模型无法满足固定输出契约，Adapter 必须明确拒绝请求，不能把私有参数传回 Rust 业务服务。业务服务的稳定协议无需跟着供应商变化。
