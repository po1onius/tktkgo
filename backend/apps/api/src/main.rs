use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderName, Method, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use reqwest::Client;
use serde::Serialize;
use serde_json::json;
use tktkgo_app::{
    AppError, Repository, Settings, create_pool,
    domain::{CreateProjectRequest, Project, ScriptReviewInput, WorkflowInput},
    init_logging,
    providers::{ModelGatewayClient, ProviderCatalog, ProviderOption},
    run_migrations,
};
use tower_http::{
    cors::{Any, CorsLayer},
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    services::ServeDir,
    trace::TraceLayer,
};
use tracing::{info, instrument, warn};
use uuid::Uuid;

#[derive(Clone)]
struct ApiState {
    repository: Repository,
    http: Client,
    gateway: ModelGatewayClient,
    settings: Settings,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let settings = Settings::from_env()?;
    init_logging("api", &settings.log_root)?;
    let result = run(settings).await;
    if let Err(error) = &result {
        tracing::error!(error = %error, "tktkgo API 异常退出");
    }
    result
}

async fn run(settings: Settings) -> Result<(), Box<dyn std::error::Error>> {
    validate_web_root(&settings).await?;
    run_migrations(settings.database_url.clone()).await?;
    let repository = Repository::new(create_pool(&settings.database_url).await?);
    let state = Arc::new(ApiState {
        repository,
        http: Client::new(),
        gateway: ModelGatewayClient::new(&settings)?,
        settings: settings.clone(),
    });

    let request_id_header = HeaderName::from_static("x-request-id");
    // Remotion 的渲染页面由内部 HTTP 服务提供，与 API 的 8000 端口不是同源。
    // 素材本身是通过 public_url 暴露的公开只读资源，因此仅在 /assets 上允许跨域读取；
    // Range 请求和对应响应头是 @remotion/media 随机访问 WAV/视频数据所必需的。
    let asset_routes = Router::<Arc<ApiState>>::new()
        .fallback_service(ServeDir::new(&settings.asset_root))
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods([Method::GET, Method::HEAD])
                .allow_headers([header::RANGE])
                .expose_headers([
                    header::ACCEPT_RANGES,
                    header::CONTENT_LENGTH,
                    header::CONTENT_RANGE,
                    header::CONTENT_TYPE,
                ]),
        );
    let app = Router::new()
        .route("/health", get(health))
        .route("/v1/providers", get(list_providers))
        .route("/v1/projects", post(create_project))
        .route("/v1/projects/{project_id}", get(get_project))
        .route("/v1/projects/{project_id}/scenes", get(list_scenes))
        .route("/v1/projects/{project_id}/version", get(get_latest_version))
        .route(
            "/v1/projects/{project_id}/renders/latest",
            get(get_latest_render),
        )
        .route("/v1/projects/{project_id}/generate", post(start_generation))
        .route(
            "/v1/projects/{project_id}/script-review",
            post(review_script),
        )
        .nest("/assets", asset_routes)
        // API 路由优先匹配，其余请求交给 Next.js 的静态导出目录处理。
        // ServeDir 会自动为根路径返回 index.html，并为不存在的文件保留正确的 404。
        .fallback_service(ServeDir::new(&settings.web_root))
        .layer(PropagateRequestIdLayer::new(request_id_header.clone()))
        .layer(SetRequestIdLayer::new(request_id_header, MakeRequestUuid))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(settings.api_addr).await?;
    info!(
        address = %settings.api_addr,
        web_root = %settings.web_root.display(),
        asset_root = %settings.asset_root.display(),
        asset_cors = "public-read-only",
        "tktkgo API 与 Web 已启动"
    );
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    info!("tktkgo API 已停止");
    Ok(())
}

/// Web 构建产物是 API 的必要运行资源。配置或构建缺失时立即终止启动，
/// 避免 API 看似健康、实际访问首页却只能得到 404 的不完整运行状态。
async fn validate_web_root(settings: &Settings) -> Result<(), AppError> {
    let index_path = settings.web_root.join("index.html");
    let metadata = tokio::fs::metadata(&index_path).await.map_err(|error| {
        AppError::Config(format!(
            "Web 构建产物不存在或不可读：{}；请先在项目根目录执行 pnpm --filter @tktkgo/web build：{error}",
            index_path.display()
        ))
    })?;
    if !metadata.is_file() {
        return Err(AppError::Config(format!(
            "Web 入口不是普通文件：{}；请重新执行 pnpm --filter @tktkgo/web build",
            index_path.display()
        )));
    }
    info!(web_index = %index_path.display(), "Web 静态构建产物校验完成");
    Ok(())
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({"status": "ok", "service": "api"}))
}

/// 业务 API 只代理固定模型网关的能力目录，不再探测或理解具体模型服务。
async fn list_providers(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ProviderCatalog>, ApiError> {
    Ok(Json(fetch_provider_catalog(&state).await?))
}

async fn fetch_provider_catalog(state: &ApiState) -> Result<ProviderCatalog, ApiError> {
    state.gateway.list_models().await.map_err(Into::into)
}

#[instrument(skip(state, request), fields(title = %request.title))]
async fn create_project(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<CreateProjectRequest>,
) -> Result<(StatusCode, Json<Project>), ApiError> {
    request.validate()?;
    ensure_requested_providers_available(&state, &request).await?;
    let auto_start = request.auto_start;
    let project = state.repository.create_project(&request).await?;
    if auto_start {
        // 项目创建已经成功时，即使 Restate 暂时无法接收任务也返回这条项目记录；
        // dispatch_generation 会把它明确标为 failed，前端可以直接展示原因并重试。
        if let Err(error) = dispatch_generation(&state, project.id).await {
            warn!(project_id = %project.id, error = %error.0, "项目已创建，但自动启动工作流失败");
        }
    }
    let project = state.repository.get_project(project.id).await?;
    Ok((StatusCode::CREATED, Json(project)))
}

async fn ensure_requested_providers_available(
    state: &ApiState,
    request: &CreateProjectRequest,
) -> Result<(), ApiError> {
    let catalog = fetch_provider_catalog(state).await?;
    ensure_catalog_choice(
        "文案",
        &catalog.text,
        &request.text_provider,
        &request.text_model,
    )?;
    ensure_catalog_choice(
        "图片",
        &catalog.image,
        &request.image_provider,
        &request.image_model,
    )?;
    ensure_catalog_choice(
        "口播",
        &catalog.speech,
        &request.speech_provider,
        &request.speech_model,
    )?;
    ensure_catalog_choice(
        "字幕",
        &catalog.alignment,
        &request.alignment_provider,
        &request.alignment_model,
    )?;
    Ok(())
}

fn ensure_catalog_choice(
    capability: &str,
    options: &[ProviderOption],
    provider: &str,
    model: &str,
) -> Result<(), ApiError> {
    if options
        .iter()
        .any(|option| option.available && option.id == provider && option.model == model)
    {
        return Ok(());
    }
    Err(AppError::Conflict(format!(
        "{capability} Provider/模型当前不可用: {provider}/{model}"
    ))
    .into())
}

async fn get_project(
    State(state): State<Arc<ApiState>>,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Project>, ApiError> {
    Ok(Json(state.repository.get_project(project_id).await?))
}

async fn list_scenes(
    State(state): State<Arc<ApiState>>,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Vec<tktkgo_app::db::SceneRecord>>, ApiError> {
    Ok(Json(
        state
            .repository
            .latest_scenes_for_project(project_id)
            .await?,
    ))
}

async fn get_latest_version(
    State(state): State<Arc<ApiState>>,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Option<tktkgo_app::domain::ProjectVersion>>, ApiError> {
    Ok(Json(
        state
            .repository
            .latest_version_for_project(project_id)
            .await?,
    ))
}

async fn get_latest_render(
    State(state): State<Arc<ApiState>>,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Option<tktkgo_app::db::RenderRecord>>, ApiError> {
    Ok(Json(
        state
            .repository
            .latest_render_for_project(project_id)
            .await?,
    ))
}

async fn start_generation(
    State(state): State<Arc<ApiState>>,
    Path(project_id): Path<Uuid>,
) -> Result<(StatusCode, Json<DispatchResponse>), ApiError> {
    let workflow_id = dispatch_generation(&state, project_id).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(DispatchResponse {
            project_id,
            workflow_id,
        }),
    ))
}

async fn review_script(
    State(state): State<Arc<ApiState>>,
    Path(project_id): Path<Uuid>,
    Json(input): Json<ScriptReviewInput>,
) -> Result<StatusCode, ApiError> {
    let project = state.repository.get_project(project_id).await?;
    if project.status != "waiting_script_review" {
        return Err(AppError::Conflict(format!("项目状态为 {}", project.status)).into());
    }
    let workflow_id = project
        .active_workflow_id
        .ok_or_else(|| AppError::Conflict("项目没有活动工作流".into()))?;
    let url = format!(
        "{}/VideoGenerationWorkflow/{}/review/send",
        state.settings.restate_ingress_url, workflow_id
    );
    let response = state
        .http
        .post(url)
        .header("idempotency-key", format!("review-{workflow_id}"))
        .json(&input)
        .send()
        .await
        .map_err(|err| AppError::external("restate", err.to_string()))?;
    if !response.status().is_success() {
        return Err(AppError::external(
            "restate",
            format!(
                "审核信号提交失败: HTTP {} {}",
                response.status(),
                response.text().await.unwrap_or_default()
            ),
        )
        .into());
    }
    info!(project_id = %project_id, workflow_id = %workflow_id, approved = input.approved, "脚本审核信号已提交");
    Ok(StatusCode::ACCEPTED)
}

#[instrument(skip(state), fields(project_id = %project_id))]
async fn dispatch_generation(state: &ApiState, project_id: Uuid) -> Result<Uuid, ApiError> {
    let project = state.repository.get_project(project_id).await?;
    let catalog = fetch_provider_catalog(state).await?;
    ensure_catalog_choice(
        "文案",
        &catalog.text,
        &project.text_provider,
        &project.text_model,
    )?;
    ensure_catalog_choice(
        "图片",
        &catalog.image,
        &project.image_provider,
        &project.image_model,
    )?;
    ensure_catalog_choice(
        "口播",
        &catalog.speech,
        &project.speech_provider,
        &project.speech_model,
    )?;
    ensure_catalog_choice(
        "字幕",
        &catalog.alignment,
        &project.alignment_provider,
        &project.alignment_model,
    )?;
    // Repository 使用条件 UPDATE 原子校验项目状态，避免并发请求同时通过检查。
    let workflow_id = Uuid::new_v4();
    state
        .repository
        .queue_project(project_id, workflow_id)
        .await?;
    let url = format!(
        "{}/VideoGenerationWorkflow/{}/run/send",
        state.settings.restate_ingress_url, workflow_id
    );
    // Workflow Key 已经是 Restate Workflow Handler 的幂等边界；Workflow 请求如果
    // 同时携带 idempotency-key Header，Restate Ingress 会明确返回 400。
    let response = state
        .http
        .post(url)
        .json(&WorkflowInput { project_id })
        .send()
        .await;
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            state
                .repository
                .finalize_project(project_id, workflow_id, "failed", Some("无法连接 Restate"))
                .await?;
            return Err(AppError::external("restate", error.to_string()).into());
        }
    };
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        state
            .repository
            .finalize_project(
                project_id,
                workflow_id,
                "failed",
                Some("无法提交到 Restate"),
            )
            .await?;
        return Err(AppError::external("restate", format!("HTTP {status}: {body}")).into());
    }
    info!(project_id = %project_id, workflow_id = %workflow_id, "视频生成工作流已提交");
    Ok(workflow_id)
}

#[derive(Serialize)]
struct DispatchResponse {
    project_id: Uuid,
    workflow_id: Uuid,
}

#[derive(Serialize)]
struct ErrorResponse {
    code: &'static str,
    message: String,
}

struct ApiError(AppError);
impl From<AppError> for ApiError {
    fn from(value: AppError) -> Self {
        Self(value)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code) = match &self.0 {
            AppError::Validation(_) => (StatusCode::BAD_REQUEST, "validation_error"),
            AppError::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
            AppError::Conflict(_) => (StatusCode::CONFLICT, "conflict"),
            AppError::Config(_) => (StatusCode::INTERNAL_SERVER_ERROR, "configuration_error"),
            AppError::External { .. } => (StatusCode::BAD_GATEWAY, "upstream_error"),
            _ => (StatusCode::INTERNAL_SERVER_ERROR, "internal_error"),
        };
        tracing::error!(error = %self.0, code, "API 请求处理失败");
        (
            status,
            Json(ErrorResponse {
                code,
                message: self.0.to_string(),
            }),
        )
            .into_response()
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("安装 Ctrl+C 信号处理器失败")
    };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("安装 SIGTERM 信号处理器失败")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = terminate => {} }
}
