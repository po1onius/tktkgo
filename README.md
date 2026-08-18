# tktkgo

将一句主题或几条大纲转换为可编辑、可重复生成的视频项目。系统使用 AI 生成文稿、分镜、插图和口播，最终通过 Remotion 确定性渲染视频。

## 架构概览

```text
Web -> Rust API -> PostgreSQL
                 -> Restate Ingress -> Rust Workflow Service
                                          -> OpenAI（文稿、分镜、插图）
                                          -> OpenAI 或本地音频 Provider
                                          -> Asset Store
                                          -> Remotion Render Service
```

后端采用 Rust、Axum、Diesel Async 和 PostgreSQL。Restate 负责持久化流程编排，Remotion 只接收经过校验的 `RenderSpec`，不会执行模型生成的代码。

生成能力通过 `TextProvider`、`ImageProvider`、`SpeechProvider` 和 `TranscriptionProvider` 四个独立接口注入 Pipeline。文稿和插图当前使用 OpenAI；口播可以选择 OpenAI 或 CosyVoice 3，词级字幕可以选择 OpenAI 或 faster-whisper。口播和字幕 Provider 由前端按项目选择并持久化，Workflow 运行时从注册表解析对应实现。

## 目录

- `backend/crates/app`：领域模型、Diesel 仓储、AI/存储/渲染 Provider。
- `backend/apps/api`：项目管理 HTTP API。
- `backend/apps/workflow`：Restate 持久化视频生成工作流。
- `backend/migrations`：无外键的 PostgreSQL 迁移。
- `render`：Remotion 模板与独立渲染服务。
- `web`：项目创建和进度查看界面。

## 本地启动

要求：Rust 1.97+、Node.js 24+、pnpm 11+、Podman、Podman Compose、FFmpeg/ffprobe。PostgreSQL 18 和 Restate 1.7 通过 `compose.yaml` 运行，其余应用服务直接在宿主机构建和启动。

1. 执行 `make`。第一次运行会创建 `.env` 并提示填写 `TKTKGO_OPENAI_API_KEY`。
2. 填写配置后再次执行 `make`，即可启动全部服务。
3. 按 `Ctrl+C` 会停止本地应用和 Podman Compose 基础服务，数据库及 Restate 数据卷会保留。

默认 `dev` 目标会使用 Podman Compose 启动 PostgreSQL 和 Restate，在宿主机安装依赖并构建 Rust、Web 和 Render 服务，随后启动全部应用服务并自动注册 Restate Workflow 端点。API 和 Workflow 启动时会自动执行嵌入式数据库迁移，不需要安装 Diesel CLI。本地模型不属于应用进程生命周期，由独立的 `models` 目标管理。

API 默认监听 `http://localhost:8000`，Restate 服务端点监听 `http://localhost:9080`，渲染服务监听 `http://localhost:8090`，Web 监听 `http://localhost:3000`。

### 使用本地口播和字幕模型

在另一个终端执行 `make models` 启动本地 Provider。该目标只读取 `local-providers/.env`，不会读取项目根目录的应用 `.env`；第一次运行会从独立示例创建配置文件。应用和模型服务可以按任意顺序启动、独立停止。

新建项目时直接在前端选择口播和字幕 Provider。页面通过 `/v1/providers` 检查本地服务，只允许选择当前可用的实现，并把 Provider 上报的实际模型名保存到项目。CosyVoice 依赖官方仓库、独立的 Python 3.10 环境和本地参考音频，需要先按 [`local-providers/README.md`](local-providers/README.md) 完成一次性准备。两个本地模型都延迟到首次实际请求时加载。文本与图片仍使用 OpenAI，因此应用仍需配置 `TKTKGO_OPENAI_API_KEY`。

## 生成流程

1. 创建项目并启动 Workflow。
2. 结构化生成 `ScriptSpec` 并保存项目版本。
3. 如果项目要求审核，Workflow 使用持久化 Promise 等待审核信号。
4. 生成 `StoryboardSpec` 和场景记录。
5. 每个场景生成插图、WAV 口播和单词级字幕。
6. ffprobe 读取真实口播时长，生成确定性的 `RenderSpec`。
7. 调用 Remotion 服务渲染 MP4，并保存 Render 记录。

每个外部步骤都有稳定的幂等键，成功结果保存在 `generation_tasks`。日志统一包含 `project_id`、`workflow_id`、`scene_id`、`stage`、`provider` 和耗时信息。

## 数据库约束

项目按要求不使用数据库外键。跨表关系通过 UUID、唯一索引和事务维护；删除项目时由应用服务显式清理关联数据。

## Restate Rust SDK 说明

当前锁定 `restate-sdk = 0.11.1`，对应 Restate Server 1.7。Rust SDK 仍在积极开发，可能出现跨版本 API 变化，因此 Restate 宏和上下文只存在于 `backend/apps/workflow`，核心业务代码不依赖 Restate。

## AGENTS
项目暂时没有生产数据，migration归一到第一个即可
