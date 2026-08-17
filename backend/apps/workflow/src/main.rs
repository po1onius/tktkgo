use std::{sync::Arc, time::Duration};

use restate_sdk::prelude::*;
use tktkgo_app::{
    Repository, Settings, create_pool,
    domain::{ScriptReviewInput, WorkflowInput, WorkflowResult},
    pipeline::PipelineService,
    providers::OpenAiProvider,
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
        info!(workflow_id = %workflow_id, project_id = %input.project_id, "视频生成工作流开始");

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

        set_status(
            &ctx,
            self.pipeline.repository().clone(),
            project.id,
            "generating_script",
            None,
        )
        .await?;
        let repository = self.pipeline.repository().clone();
        // ID 由 Restate 上下文生成，重放时保持稳定；版本号在读取项目后确定，避免数据库提交成功但日志未确认时创建重复版本。
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

        let pipeline = self.pipeline.clone();
        let script_project = project.clone();
        let script_version = version.clone();
        let script = match ctx
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
                return fail_workflow(&ctx, self.pipeline.repository().clone(), project.id, err)
                    .await;
            }
        };

        if project.require_script_review {
            set_status(
                &ctx,
                self.pipeline.repository().clone(),
                project.id,
                "waiting_script_review",
                None,
            )
            .await?;
            info!(workflow_id = %workflow_id, project_id = %project.id, "等待人工审核脚本");
            let Json(review) = ctx
                .promise::<Json<ScriptReviewInput>>("script-review")
                .await?;
            if !review.approved {
                let reason = review.feedback.unwrap_or_else(|| "脚本审核未通过".into());
                set_status(
                    &ctx,
                    self.pipeline.repository().clone(),
                    project.id,
                    "draft",
                    Some(reason.clone()),
                )
                .await?;
                info!(workflow_id = %workflow_id, project_id = %project.id, reason, "脚本被退回，当前工作流正常结束，可重新发起新版本");
                return Ok(Json(WorkflowResult {
                    project_id: project.id,
                    version: version.version,
                    outcome: "script_rejected".into(),
                    render_url: None,
                }));
            }
        }

        set_status(
            &ctx,
            self.pipeline.repository().clone(),
            project.id,
            "generating_storyboard",
            None,
        )
        .await?;
        let pipeline = self.pipeline.clone();
        let board_project = project.clone();
        let board_version = version.clone();
        let storyboard = match ctx
            .run(move || async move {
                pipeline
                    .generate_storyboard(workflow_id, &board_project, &board_version, &script)
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
                return fail_workflow(&ctx, self.pipeline.repository().clone(), project.id, err)
                    .await;
            }
        };

        set_status(
            &ctx,
            self.pipeline.repository().clone(),
            project.id,
            "generating_assets",
            None,
        )
        .await?;
        let repository = self.pipeline.repository().clone();
        let version_id = version.id;
        let Json(scenes) = ctx
            .run(move || async move {
                repository
                    .list_scenes(version_id)
                    .await
                    .map(Json)
                    .map_err(HandlerError::from)
            })
            .name("load-scenes")
            .retry_policy(database_retry())
            .await?;
        let mut generated = Vec::with_capacity(scenes.len());
        for scene in scenes {
            let pipeline = self.pipeline.clone();
            let scene_project = project.clone();
            let scene_version = version.clone();
            let style = storyboard.style_bible.clone();
            let stage_name = format!("generate-scene-{}", scene.sequence);
            let output = match ctx
                .run(move || async move {
                    pipeline
                        .generate_scene_assets(
                            workflow_id,
                            &scene_project,
                            &scene_version,
                            &scene,
                            &style,
                        )
                        .await
                        .map(Json)
                        .map_err(HandlerError::from)
                })
                .name(stage_name)
                .retry_policy(external_retry())
                .await
            {
                Ok(Json(output)) => output,
                Err(err) => {
                    return fail_workflow(
                        &ctx,
                        self.pipeline.repository().clone(),
                        project.id,
                        err,
                    )
                    .await;
                }
            };
            generated.push(output);
        }

        set_status(
            &ctx,
            self.pipeline.repository().clone(),
            project.id,
            "building_timeline",
            None,
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

        set_status(
            &ctx,
            self.pipeline.repository().clone(),
            project.id,
            "rendering",
            None,
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
                return fail_workflow(&ctx, self.pipeline.repository().clone(), project.id, err)
                    .await;
            }
        };

        set_status(
            &ctx,
            self.pipeline.repository().clone(),
            project.id,
            "completed",
            None,
        )
        .await?;
        info!(workflow_id = %workflow_id, project_id = %project.id, version = version.version, render_url, "视频生成工作流完成");
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
    status: &'static str,
    error_message: Option<String>,
) -> Result<(), TerminalError> {
    ctx.run(move || async move {
        repository
            .update_project_status(project_id, status, error_message.as_deref())
            .await
            .map_err(HandlerError::from)
    })
    .name(format!("status-{status}"))
    .retry_policy(database_retry())
    .await
}

async fn fail_workflow(
    ctx: &WorkflowContext<'_>,
    repository: Repository,
    project_id: Uuid,
    cause: TerminalError,
) -> HandlerResult<Json<WorkflowResult>> {
    let message = cause.to_string();
    error!(project_id = %project_id, error = %message, "视频生成工作流失败");
    let saved_message = message.clone();
    ctx.run(move || async move {
        repository
            .update_project_status(project_id, "failed", Some(&saved_message))
            .await
            .map_err(HandlerError::from)
    })
    .name("mark-workflow-failed")
    .retry_policy(database_retry())
    .await?;
    Err(TerminalError::new_with_code(cause.code(), message).into())
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
    let ai = Arc::new(OpenAiProvider::new(&settings)?);
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
        ai,
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
