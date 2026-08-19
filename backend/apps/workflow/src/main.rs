use std::{sync::Arc, time::Duration};

use restate_sdk::prelude::*;
use tktkgo_app::{
    Repository, Settings, create_pool,
    domain::{
        RenderSpec, ScriptReviewInput, ScriptSpec, StoryboardSpec, WorkflowInput, WorkflowResult,
    },
    pipeline::PipelineService,
    providers::ModelGatewayClient,
    render::RenderClient,
    run_migrations,
    storage::LocalAssetStore,
};
use tracing::{error, info};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use uuid::Uuid;

#[derive(Clone)]
struct VideoGenerationWorkflow {
    pipeline: PipelineService,
}

#[restate_sdk::workflow]
impl VideoGenerationWorkflow {
    #[handler]
    async fn run(
        &self,
        mut ctx: WorkflowContext<'_>,
        Json(input): Json<WorkflowInput>,
    ) -> HandlerResult<Json<WorkflowResult>> {
        let workflow_id = Uuid::parse_str(ctx.key()).terminal()?;
        let job_id = input.job_id;
        info!(workflow_id = %workflow_id, job_id = %job_id, project_id = %input.project_id, resume_version_id = ?input.resume_version_id, "视频生成工作流开始");

        let repository = self.pipeline.repository().clone();
        let project_id = input.project_id;
        let Json(project) = ctx
            .run(move || async move {
                repository
                    .get_project(project_id)
                    .await
                    .map(Json)
                    .map_err(HandlerError::from)
            })
            .name("load-project")
            .retry_policy(database_retry())
            .await?;
        if project.active_workflow_id != Some(workflow_id) {
            return Err(TerminalError::new_with_code(409, "工作流不是项目当前活动工作流").into());
        }
        let repository = self.pipeline.repository().clone();
        let Json(job) = ctx
            .run(move || async move {
                repository
                    .get_generation_job(job_id)
                    .await
                    .map(Json)
                    .map_err(HandlerError::from)
            })
            .name("load-generation-job")
            .retry_policy(database_retry())
            .await?;
        if job.project_id != project.id || job.workflow_id != workflow_id {
            return Err(
                TerminalError::new_with_code(409, "生成任务与项目当前 Workflow 不匹配").into(),
            );
        }

        let version = match input.resume_version_id {
            Some(version_id) => {
                if job.project_version_id != Some(version_id) {
                    return Err(TerminalError::new_with_code(
                        409,
                        "继续版本与生成任务绑定版本不一致",
                    )
                    .into());
                }
                let repository = self.pipeline.repository().clone();
                let Json(version) = ctx
                    .run(move || async move {
                        repository
                            .get_project_version(version_id)
                            .await
                            .map(Json)
                            .map_err(HandlerError::from)
                    })
                    .name("load-resume-version")
                    .retry_policy(database_retry())
                    .await?;
                if version.project_id != project.id || version.version != project.current_version {
                    return Err(TerminalError::new_with_code(409, "只能继续项目当前版本").into());
                }
                info!(job_id = %job_id, workflow_id = %workflow_id, version_id = %version.id, version = version.version, "继续任务已加载原项目版本");
                version
            }
            None => {
                set_status(
                    &ctx,
                    self.pipeline.repository().clone(),
                    project.id,
                    job_id,
                    workflow_id,
                    "generating_script",
                )
                .await?;
                let repository = self.pipeline.repository().clone();
                // ID 由 Restate 上下文生成，重放时保持稳定；数据库写入和任务绑定各自
                // 作为 durable step，任一步骤重放都保持幂等。
                let version_id = ctx.rand_uuid();
                let next_version = project.current_version + 1;
                let Json(version) = ctx
                    .run(move || async move {
                        repository
                            .create_project_version(project_id, version_id, next_version)
                            .await
                            .map(Json)
                            .map_err(HandlerError::from)
                    })
                    .name("create-project-version")
                    .retry_policy(database_retry())
                    .await?;
                let repository = self.pipeline.repository().clone();
                ctx.run(move || async move {
                    repository
                        .assign_generation_job_version(job_id, workflow_id, version_id)
                        .await
                        .map_err(HandlerError::from)
                })
                .name("attach-job-version")
                .retry_policy(database_retry())
                .await?;
                version
            }
        };

        // render_spec 是最远检查点。它存在时说明脚本、审核、分镜和全部素材均已完成，
        // 继续任务应直接渲染，避免即使命中缓存也遍历全部场景。
        let spec = if let Some(saved) = version.render_spec.clone() {
            set_status(
                &ctx,
                self.pipeline.repository().clone(),
                project.id,
                job_id,
                workflow_id,
                "building_timeline",
            )
            .await?;
            let spec = decode_checkpoint::<RenderSpec>(saved, "render_spec")?;
            info!(job_id = %job_id, version_id = %version.id, scene_count = spec.scenes.len(), "命中 RenderSpec 检查点，跳过文稿、分镜和素材阶段");
            spec
        } else {
            let storyboard = if let Some(saved) = version.storyboard_spec.clone() {
                let storyboard = decode_checkpoint::<StoryboardSpec>(saved, "storyboard_spec")?;
                info!(job_id = %job_id, version_id = %version.id, scene_count = storyboard.scenes.len(), "命中分镜检查点，跳过文稿与分镜生成");
                storyboard
            } else {
                set_status(
                    &ctx,
                    self.pipeline.repository().clone(),
                    project.id,
                    job_id,
                    workflow_id,
                    "generating_script",
                )
                .await?;
                let script = if let Some(saved) = version.script_spec.clone() {
                    let script = decode_checkpoint::<ScriptSpec>(saved, "script_spec")?;
                    info!(job_id = %job_id, version_id = %version.id, "命中文稿检查点，跳过文稿模型调用");
                    script
                } else {
                    let pipeline = self.pipeline.clone();
                    let script_project = project.clone();
                    let script_version = version.clone();
                    match ctx
                        .run(move || async move {
                            pipeline
                                .generate_script(workflow_id, &script_project, &script_version)
                                .await
                                .map(Json)
                                .map_err(HandlerError::from)
                        })
                        .name("generate-script")
                        .retry_policy(external_retry())
                        .await
                    {
                        Ok(Json(script)) => script,
                        Err(err) => {
                            return fail_workflow(
                                &ctx,
                                self.pipeline.repository().clone(),
                                project.id,
                                job_id,
                                workflow_id,
                                "script",
                                err,
                            )
                            .await;
                        }
                    }
                };

                if project.require_script_review && version.script_review_status != "approved" {
                    set_status(
                        &ctx,
                        self.pipeline.repository().clone(),
                        project.id,
                        job_id,
                        workflow_id,
                        "waiting_script_review",
                    )
                    .await?;
                    info!(workflow_id = %workflow_id, job_id = %job_id, project_id = %project.id, "等待人工审核脚本");
                    let Json(review) = ctx
                        .promise::<Json<ScriptReviewInput>>("script-review")
                        .await?;
                    let repository = self.pipeline.repository().clone();
                    let feedback = review.feedback.clone();
                    let approved = review.approved;
                    ctx.run(move || async move {
                        repository
                            .mark_script_review(version.id, approved, feedback.as_deref())
                            .await
                            .map_err(HandlerError::from)
                    })
                    .name("save-script-review")
                    .retry_policy(database_retry())
                    .await?;
                    if !review.approved {
                        let reason = review.feedback.unwrap_or_else(|| "脚本审核未通过".into());
                        finish_workflow(
                            &ctx,
                            self.pipeline.repository().clone(),
                            project.id,
                            job_id,
                            workflow_id,
                            "draft",
                            "rejected",
                            "script_review",
                            Some(reason.clone()),
                        )
                        .await?;
                        info!(workflow_id = %workflow_id, job_id = %job_id, project_id = %project.id, reason, "脚本被退回，当前任务正常结束，可重新发起新版本");
                        return Ok(Json(WorkflowResult {
                            project_id: project.id,
                            version: version.version,
                            outcome: "script_rejected".into(),
                            render_url: None,
                        }));
                    }
                } else if project.require_script_review {
                    info!(job_id = %job_id, version_id = %version.id, "命中已通过的脚本审核检查点");
                }

                set_status(
                    &ctx,
                    self.pipeline.repository().clone(),
                    project.id,
                    job_id,
                    workflow_id,
                    "generating_storyboard",
                )
                .await?;
                let pipeline = self.pipeline.clone();
                let board_project = project.clone();
                let board_version = version.clone();
                match ctx
                    .run(move || async move {
                        pipeline
                            .generate_storyboard(
                                workflow_id,
                                &board_project,
                                &board_version,
                                &script,
                            )
                            .await
                            .map(Json)
                            .map_err(HandlerError::from)
                    })
                    .name("generate-storyboard")
                    .retry_policy(external_retry())
                    .await
                {
                    Ok(Json(storyboard)) => storyboard,
                    Err(err) => {
                        return fail_workflow(
                            &ctx,
                            self.pipeline.repository().clone(),
                            project.id,
                            job_id,
                            workflow_id,
                            "storyboard",
                            err,
                        )
                        .await;
                    }
                }
            };

            set_status(
                &ctx,
                self.pipeline.repository().clone(),
                project.id,
                job_id,
                workflow_id,
                "generating_assets",
            )
            .await?;
            let repository = self.pipeline.repository().clone();
            let version_id = version.id;
            let project_id = project.id;
            let drafts = storyboard.scenes.clone();
            let expected_scene_count = drafts.len();
            let Json(scenes) = ctx
                .run(move || async move {
                    let existing = repository.list_scenes(version_id).await?;
                    if existing.len() == expected_scene_count {
                        return Ok(Json(existing));
                    }
                    info!(version_id = %version_id, existing_count = existing.len(), expected_scene_count, "分镜记录与检查点不一致，将从 StoryboardSpec 原子恢复");
                    repository
                        .replace_scenes(project_id, version_id, &drafts)
                        .await
                        .map(Json)
                        .map_err(HandlerError::from)
                })
                .name("load-or-restore-scenes")
                .retry_policy(database_retry())
                .await?;
            let pipeline = self.pipeline.clone();
            let scene_project = project.clone();
            let scene_version = version.clone();
            let style = storyboard.style_bible.clone();
            let generated = match ctx
                .run(move || async move {
                    pipeline
                        .generate_all_scene_assets(
                            workflow_id,
                            scene_project,
                            scene_version,
                            scenes,
                            style,
                        )
                        .await
                        .map(Json)
                        .map_err(HandlerError::from)
                })
                .name("generate-scene-assets")
                .retry_policy(external_retry())
                .await
            {
                Ok(Json(output)) => output,
                Err(err) => {
                    return fail_workflow(
                        &ctx,
                        self.pipeline.repository().clone(),
                        project.id,
                        job_id,
                        workflow_id,
                        "assets",
                        err,
                    )
                    .await;
                }
            };

            set_status(
                &ctx,
                self.pipeline.repository().clone(),
                project.id,
                job_id,
                workflow_id,
                "building_timeline",
            )
            .await?;
            // RenderSpec 是纯函数计算；结果随后写入 Restate 日志和数据库，重放时完全一致。
            let spec = self
                .pipeline
                .build_render_spec(&project, &version, generated);
            let repository = self.pipeline.repository().clone();
            let save_version_id = version.id;
            let spec_to_save = spec.clone();
            ctx.run(move || async move {
                repository
                    .save_render_spec(save_version_id, &spec_to_save)
                    .await
                    .map_err(HandlerError::from)
            })
            .name("save-render-spec")
            .retry_policy(database_retry())
            .await?;
            spec
        };

        set_status(
            &ctx,
            self.pipeline.repository().clone(),
            project.id,
            job_id,
            workflow_id,
            "rendering",
        )
        .await?;
        let pipeline = self.pipeline.clone();
        let render_project = project.clone();
        let render_version = version.clone();
        let render_url = match ctx
            .run(move || async move {
                pipeline
                    .render_video(workflow_id, &render_project, &render_version, spec)
                    .await
                    .map_err(HandlerError::from)
            })
            .name("render-video")
            .retry_policy(render_retry())
            .await
        {
            Ok(url) => url,
            Err(err) => {
                return fail_workflow(
                    &ctx,
                    self.pipeline.repository().clone(),
                    project.id,
                    job_id,
                    workflow_id,
                    "render",
                    err,
                )
                .await;
            }
        };

        finish_workflow(
            &ctx,
            self.pipeline.repository().clone(),
            project.id,
            job_id,
            workflow_id,
            "completed",
            "completed",
            "completed",
            None,
        )
        .await?;
        info!(workflow_id = %workflow_id, job_id = %job_id, project_id = %project.id, version = version.version, render_url, "视频生成工作流完成");
        Ok(Json(WorkflowResult {
            project_id: project.id,
            version: version.version,
            outcome: "completed".into(),
            render_url: Some(render_url),
        }))
    }

    #[handler]
    async fn review(
        &self,
        ctx: SharedWorkflowContext<'_>,
        Json(input): Json<ScriptReviewInput>,
    ) -> HandlerResult<()> {
        info!(
            workflow_id = ctx.key(),
            approved = input.approved,
            "收到脚本审核信号"
        );
        ctx.resolve_promise("script-review", Json(input));
        Ok(())
    }
}

async fn set_status(
    ctx: &WorkflowContext<'_>,
    repository: Repository,
    project_id: Uuid,
    job_id: Uuid,
    workflow_id: Uuid,
    status: &'static str,
) -> Result<(), TerminalError> {
    ctx.run(move || async move {
        repository
            .update_generation_status(
                project_id,
                job_id,
                workflow_id,
                status,
                status,
                status,
                None,
                None,
                false,
                false,
            )
            .await
            .map_err(HandlerError::from)
    })
    .name(format!("status-{status}"))
    .retry_policy(database_retry())
    .await
}

#[allow(clippy::too_many_arguments)]
async fn finish_workflow(
    ctx: &WorkflowContext<'_>,
    repository: Repository,
    project_id: Uuid,
    job_id: Uuid,
    workflow_id: Uuid,
    project_status: &'static str,
    job_status: &'static str,
    stage: &'static str,
    error_message: Option<String>,
) -> Result<(), TerminalError> {
    ctx.run(move || async move {
        repository
            .update_generation_status(
                project_id,
                job_id,
                workflow_id,
                project_status,
                job_status,
                stage,
                None,
                error_message.as_deref(),
                false,
                true,
            )
            .await
            .map_err(HandlerError::from)
    })
    .name(format!("finish-{job_status}"))
    .retry_policy(database_retry())
    .await
}

async fn fail_workflow(
    ctx: &WorkflowContext<'_>,
    repository: Repository,
    project_id: Uuid,
    job_id: Uuid,
    workflow_id: Uuid,
    stage: &'static str,
    cause: TerminalError,
) -> HandlerResult<Json<WorkflowResult>> {
    let message = cause.to_string();
    let code = cause.code().to_string();
    error!(project_id = %project_id, job_id = %job_id, stage, error_code = %code, error = %message, "视频生成工作流失败");
    let saved_message = message.clone();
    let saved_code = code.clone();
    ctx.run(move || async move {
        repository
            .update_generation_status(
                project_id,
                job_id,
                workflow_id,
                "failed",
                "failed",
                stage,
                Some(&saved_code),
                Some(&saved_message),
                true,
                true,
            )
            .await
            .map_err(HandlerError::from)
    })
    .name("mark-workflow-failed")
    .retry_policy(database_retry())
    .await?;
    Err(TerminalError::new_with_code(cause.code(), message).into())
}

fn decode_checkpoint<T: serde::de::DeserializeOwned>(
    value: serde_json::Value,
    name: &str,
) -> Result<T, TerminalError> {
    serde_json::from_value(value).map_err(|error| {
        TerminalError::new_with_code(500, format!("{name} 检查点无法反序列化: {error}"))
    })
}

fn database_retry() -> RunRetryPolicy {
    RunRetryPolicy::default()
        .initial_delay(Duration::from_millis(200))
        .exponentiation_factor(2.0)
        .max_delay(Duration::from_secs(5))
        .max_attempts(8)
        .max_duration(Duration::from_secs(60))
}

fn external_retry() -> RunRetryPolicy {
    RunRetryPolicy::default()
        .initial_delay(Duration::from_secs(1))
        .exponentiation_factor(2.0)
        .max_delay(Duration::from_secs(30))
        .max_attempts(5)
        .max_duration(Duration::from_secs(20 * 60))
}

fn render_retry() -> RunRetryPolicy {
    RunRetryPolicy::default()
        .initial_delay(Duration::from_secs(2))
        .exponentiation_factor(2.0)
        .max_delay(Duration::from_secs(60))
        .max_attempts(3)
        .max_duration(Duration::from_secs(45 * 60))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_logging();
    let settings = Settings::from_env()?;
    run_migrations(settings.database_url.clone()).await?;
    let repository = Repository::new(create_pool(&settings.database_url).await?);
    // Workflow 只依赖固定的 Model Gateway 协议，不包含任何模型厂商 SDK 或私有参数。
    let gateway = ModelGatewayClient::new(&settings)?;
    info!(
        model_gateway_url = settings.model_gateway_url,
        "固定模型能力客户端初始化完成"
    );
    let storage = Arc::new(
        LocalAssetStore::new(
            settings.asset_root.clone(),
            settings.public_asset_base_url.clone(),
        )
        .await?,
    );
    let renderer = RenderClient::new(settings.renderer_url.clone());
    let pipeline = PipelineService::new(
        repository,
        gateway,
        storage,
        renderer,
        settings.asset_root.clone(),
    );

    info!(address = %settings.workflow_addr, "tktkgo Restate 工作流服务已启动");
    HttpServer::new(
        Endpoint::builder()
            .bind(VideoGenerationWorkflow { pipeline })
            .build(),
    )
    .listen_and_serve(settings.workflow_addr)
    .await;
    Ok(())
}

fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_env("TKTKGO_LOG")
        .unwrap_or_else(|_| "info,tktkgo=debug".into());
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().json().flatten_event(true))
        .init();
}
