# tktkgo

将一句主题或几条大纲转换为可编辑、可重复生成的视频项目。系统使用 AI 生成文稿、分镜、插图和口播，最终通过 Remotion 确定性渲染视频。

## 架构概览

```text
Web -> Rust API -> PostgreSQL
                 -> Restate Ingress -> Rust Workflow Service
                                          -> OpenAI
                                          -> Asset Store
                                          -> Remotion Render Service
```

后端采用 Rust、Axum、Diesel Async 和 PostgreSQL。Restate 负责持久化流程编排，Remotion 只接收经过校验的 `RenderSpec`，不会执行模型生成的代码。

## 目录

- `backend/crates/app`：领域模型、Diesel 仓储、AI/存储/渲染 Provider。
- `backend/apps/api`：项目管理 HTTP API。
- `backend/apps/workflow`：Restate 持久化视频生成工作流。
- `backend/migrations`：无外键的 PostgreSQL 迁移。
- `render`：Remotion 模板与独立渲染服务。
- `web`：项目创建和进度查看界面。

## 本地启动

要求：Rust 1.97+、Node.js 24+、pnpm 11+、PostgreSQL 18、Restate 1.7、FFmpeg/ffprobe。建议安装 Docker Compose；当前 Codex 执行环境没有 Docker，需要在你的开发机手动安装。

1. 复制配置：`cp .env.example .env`，填写 `TKTKGO_OPENAI_API_KEY`。
2. 启动基础设施：`docker compose up -d postgres restate`。
3. 执行迁移：`diesel migration run --migration-dir backend/migrations`。
4. 启动 API：`cargo run -p tktkgo-api`。
5. 启动工作流服务：`cargo run -p tktkgo-workflow`。
6. 注册工作流端点：`curl -X POST localhost:9070/deployments -H 'content-type: application/json' -d '{"uri":"http://host.docker.internal:9080"}'`。Linux 下请将地址替换为 Restate 容器能访问的宿主机地址。
7. 安装并启动前端/渲染服务：`pnpm install && pnpm dev`。

API 默认监听 `http://localhost:8000`，Restate 服务端点监听 `http://localhost:9080`，渲染服务监听 `http://localhost:8090`，Web 监听 `http://localhost:3000`。

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

