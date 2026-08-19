# tktkgo

将一句主题或几条大纲转换为可审核、可重复生成的视频项目。系统使用 AI 生成文稿、分镜、插图和口播，最终通过 Remotion 确定性渲染视频。

## 架构概览

```text
Browser -> Rust API（托管静态 Web） -> PostgreSQL
                                  -> Restate Ingress -> Rust Workflow Service
                                                           -> 固定 Provider Gateway API
                                                                -> OpenAI Adapter
                                                                -> DeepSeek Adapter
                                                                -> Pic2API Image Adapter
                                                                -> CosyVoice Adapter
                                                                -> faster-whisper Adapter
                                                           -> Asset Store
                                                           -> Remotion Render Service
```

后端采用 Rust、Axum、Diesel Async 和 PostgreSQL。Restate 负责持久化流程编排，Remotion 只接收经过校验的 `RenderSpec`，不会执行模型生成的代码。

Rust Pipeline 只依赖一个 `ModelGatewayClient` 和四个固定 HTTP 能力协议，不包含供应商 SDK、鉴权或私有参数。OpenAI、DeepSeek、Pic2API、CosyVoice 和 faster-whisper 的请求与响应差异全部由 Python Adapter 处理；四类能力的 Provider 和模型由前端选择并随项目持久化。

图片协议接收“提示词、目标宽高、质量”，始终输出目标尺寸的 PNG 二进制。供应商只生成最接近目标画幅的标准尺寸，Adapter 再按 EXIF 方向矫正、等比缩放并居中裁剪，Rust 和 Remotion 不承担模型尺寸兼容逻辑。

## 目录

- `backend/crates/app`：领域模型、Diesel 仓储、AI/存储/渲染 Provider。
- `backend/apps/api`：项目管理 HTTP API。
- `backend/apps/workflow`：Restate 持久化视频生成工作流。
- `backend/migrations`：无外键的 PostgreSQL 迁移。
- `provider`：固定模型能力 HTTP API、供应商适配层和独立网关配置。
- `local-models`：完全独立部署的 CosyVoice、faster-whisper 模型执行服务。
- `render`：Remotion 模板与独立渲染服务。
- `web`：项目创建和进度查看界面；Next.js 构建为静态文件后由 Rust API 托管。

## 本地启动

要求：Rust 1.97+、Node.js 24+、pnpm 11+、uv、Podman、Podman Compose、FFmpeg/ffprobe。PostgreSQL 18 和 Restate 1.7 通过 `compose.yaml` 运行，其余应用服务直接在宿主机构建和启动。

1. 进入 `provider`，首次部署先复制 `providers.example.toml` 为 `providers.toml`，填写所选远程 Provider 的 Key 和四类能力的可选模型；执行 `uv sync --frozen` 后使用 `uv run --frozen python provider.py` 启动网关。
2. 如果网关配置了本地模型，在各自终端进入 `local-models/faster-whisper` 或 `local-models/cosyvoice`，执行 `uv sync --frozen` 后使用 `uv run --frozen python provider.py` 启动。两者拥有独立 TOML、uv 环境和模型缓存，启动时会自动下载缺失权重；CosyVoice 首次准备官方源码和私有音色的方法见 [`local-models/README.md`](local-models/README.md)。
3. 在另一个终端执行 `make`，启动基础设施和全部业务应用。各目标独立运行和停止，数据库及 Restate 数据卷会保留。

默认 `dev` 目标会使用 Podman Compose 启动 PostgreSQL 和 Restate，在宿主机安装依赖并构建 Rust、Web 和 Render 服务，随后启动应用服务并自动注册 Restate Workflow 端点。Web 使用 Next.js 静态导出，运行时由 Rust API 同源托管，不再启动独立的 Next.js 服务。API 和 Workflow 启动时会自动执行嵌入式数据库迁移，不需要安装 Diesel CLI。Provider 网关和两个本地模型均不属于应用进程生命周期。

素材目录由 Makefile 统一设为项目根目录下的 `storage`。Makefile 会根据自身位置计算项目根目录，并向 API、Workflow 和 Remotion 导出同一个绝对路径，避免各服务工作目录不同导致素材与渲染产物被写入两套 `storage`。如需调整，可执行 `make ASSET_ROOT=/absolute/path`。

API 和 Web 默认统一监听 `http://localhost:8000`，Restate 服务端点监听 `http://localhost:9080`，渲染服务监听 `http://localhost:8090`。Node.js/pnpm 仍用于构建 Web 和运行 Remotion，但 Web 本身不需要 Node.js 运行时服务。

Rust 服务日志同时输出到终端和 `TKTKGO_LOG_ROOT`（默认 `./logs`）。`api.log` 与 `workflow.log` 保存对应服务的完整 JSON Lines 日志；包含 `workflow_id` 的事件还会写入 `logs/tasks/<workflow_id>.log`，一个视频生成任务对应一个文件，Restate 重放和阶段重试继续追加到原文件。可使用 `tail -f logs/tasks/<workflow_id>.log` 实时查看指定任务。

### 使用本地口播和字幕模型

Provider 网关进程只读取 `provider/providers.toml`，不会安装或启动本地模型；它会校验每类能力至少有一个模型、Provider 与能力匹配，以及被选中的远程 Provider 已配置 Key。本地执行服务只向网关暴露 HTTP 地址，网关通过健康响应确认实际模型和音色是否可用。

faster-whisper 进程只读取 `local-models/faster-whisper/provider.toml`；CosyVoice 进程只读取 `local-models/cosyvoice/provider.toml` 和私有音色文件。三份配置互不引用。业务应用只保存 `TKTKGO_MODEL_GATEWAY_URL`，不会接触模型 API Key 或本地模型私有参数。

新建项目时直接在前端分别选择文案、图片、口播和字幕的 Provider/模型组合。页面通过业务 API 代理的模型目录只允许选择当前可用组合。CosyVoice 的参考音频仍需人工准备，具体部署见 [`local-models/README.md`](local-models/README.md)。本地模型会在各自服务启动阶段下载并加载，健康检查成功后即可处理业务请求。

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
