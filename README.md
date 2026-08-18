# tktkgo

将一句主题或几条大纲转换为可审核、可重复生成的视频项目。系统使用 AI 生成文稿、分镜、插图和口播，最终通过 Remotion 确定性渲染视频。

## 架构概览

```text
Web -> Rust API -> PostgreSQL
                 -> Restate Ingress -> Rust Workflow Service
                                          -> 固定 Model Gateway API
                                               -> OpenAI Adapter
                                               -> CosyVoice Adapter
                                               -> faster-whisper Adapter
                                          -> Asset Store
                                          -> Remotion Render Service
```

后端采用 Rust、Axum、Diesel Async 和 PostgreSQL。Restate 负责持久化流程编排，Remotion 只接收经过校验的 `RenderSpec`，不会执行模型生成的代码。

Rust Pipeline 只依赖一个 `ModelGatewayClient` 和四个固定 HTTP 能力协议，不包含供应商 SDK、鉴权或私有参数。OpenAI、CosyVoice 和 faster-whisper 的请求与响应差异全部由 Python Adapter 处理；四类能力的 Provider 和模型由前端选择并随项目持久化。

图片协议接收“提示词、目标宽高、质量”，始终输出目标尺寸的 PNG 二进制。供应商只生成最接近目标画幅的标准尺寸，Adapter 再按 EXIF 方向矫正、等比缩放并居中裁剪，Rust 和 Remotion 不承担模型尺寸兼容逻辑。

## 目录

- `backend/crates/app`：领域模型、Diesel 仓储、AI/存储/渲染 Provider。
- `backend/apps/api`：项目管理 HTTP API。
- `backend/apps/workflow`：Restate 持久化视频生成工作流。
- `backend/migrations`：无外键的 PostgreSQL 迁移。
- `local-providers/gateway`：固定模型能力 HTTP API 与供应商适配层。
- `local-providers/cosyvoice`、`local-providers/faster-whisper`：本地模型执行服务。
- `render`：Remotion 模板与独立渲染服务。
- `web`：项目创建和进度查看界面。

## 本地启动

要求：Rust 1.97+、Node.js 24+、pnpm 11+、Podman、Podman Compose、FFmpeg/ffprobe。PostgreSQL 18 和 Restate 1.7 通过 `compose.yaml` 运行，其余应用服务直接在宿主机构建和启动。

1. 执行 `make models`，第一次运行会创建 `local-providers/.env`；填写其中的 `TKTKGO_OPENAI_API_KEY` 后重新执行。
2. 在另一个终端执行 `make`，启动基础设施和全部业务应用。
3. 两个目标独立运行，分别按 `Ctrl+C` 停止；数据库及 Restate 数据卷会保留。

默认 `dev` 目标会使用 Podman Compose 启动 PostgreSQL 和 Restate，在宿主机安装依赖并构建 Rust、Web 和 Render 服务，随后启动全部应用服务并自动注册 Restate Workflow 端点。API 和 Workflow 启动时会自动执行嵌入式数据库迁移，不需要安装 Diesel CLI。本地模型不属于应用进程生命周期，由独立的 `models` 目标管理。

API 默认监听 `http://localhost:8000`，Restate 服务端点监听 `http://localhost:9080`，渲染服务监听 `http://localhost:8090`，Web 监听 `http://localhost:3000`。

### 使用本地口播和字幕模型

`make models` 会启动固定 Model Gateway、默认的 faster-whisper HTTP 服务和可选的 CosyVoice 服务。该目标只读取 `local-providers/.env`，不会读取项目根目录的应用 `.env`。业务应用只保存 `TKTKGO_MODEL_GATEWAY_URL`，不会接触模型 API Key 或供应商私有参数。

新建项目时直接在前端分别选择文案、图片、口播和字幕的 Provider/模型组合。页面通过业务 API 代理的模型目录只允许选择当前可用组合。CosyVoice 依赖官方仓库、独立的 Python 3.10 环境和本地参考音频，需要先按 [`local-providers/README.md`](local-providers/README.md) 完成一次性准备。本地大模型延迟到第一次实际请求时加载。

## 生成流程

1. 创建项目并启动 Workflow。
2. 结构化生成 `ScriptSpec` 并保存项目版本。
3. 如果项目要求审核，Workflow 使用持久化 Promise 等待审核信号。
4. 生成 `StoryboardSpec` 和场景记录。
5. 场景以有界并发生成素材；单个场景并行生成插图和 WAV 口播，随后生成单词级字幕。
6. ffprobe 读取真实口播时长，生成确定性的 `RenderSpec`。
7. 调用 Remotion 服务渲染 MP4，并保存 Render 记录。

每种模型能力都有独立、稳定的任务 ID 和幂等键，成功结果保存在 `generation_tasks`，同一 ID 也会传给 Model Gateway 和供应商。项目状态写入绑定当前活动 Workflow，渲染产物使用完整 RenderSpec 哈希命名，并在原子替换前验证视频流与时长。日志统一包含 `project_id`、`workflow_id`、`scene_id`、`stage`、`provider` 和耗时信息。

## 数据库约束

项目按要求不使用数据库外键。跨表关系通过 UUID、唯一索引和事务维护。当前仍处于无生产数据的开发阶段，所有表结构已经归一到首个 migration；如果本地数据库运行过旧结构，请删除开发数据卷后重新启动。

```bash
podman compose down -v  # 会删除本项目的 PostgreSQL 与 Restate 开发数据
```

## Restate Rust SDK 说明

当前锁定 `restate-sdk = 0.11.1`，对应 Restate Server 1.7。Rust SDK 仍在积极开发，可能出现跨版本 API 变化，因此 Restate 宏和上下文只存在于 `backend/apps/workflow`，核心业务代码不依赖 Restate。

## AGENTS
项目暂时没有生产数据，migration归一到第一个即可
