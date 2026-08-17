use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderName, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use reqwest::Client;
use serde::Serialize;
use serde_json::json;
use tktkgo_app::{
    AppError, Repository, Settings, create_pool,
    domain::{CreateProjectRequest, Project, ScriptReviewInput, WorkflowInput},
    run_migrations,
};
use tower_http::{
    cors::{Any, CorsLayer},
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    services::ServeDir,
    trace::TraceLayer,
};
use tracing::{info, instrument};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use uuid::Uuid;

#[derive(Clone)]
struct ApiState {
    repository: Repository,
    http: Client,
    settings: Settings,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_logging();
    let settings = Settings::from_env()?;
    run_migrations(settings.database_url.clone()).await?;
    let repository = Repository::new(create_pool(&settings.database_url).await?);
    let state = Arc::new(ApiState {
        repository,
        http: Client::new(),
        settings: settings.clone(),
    });

    let request_id_header = HeaderName::from_static("x-request-id");
    let app = Router::new()
        .route("/health", get(health))
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
        .nest_service("/assets", ServeDir::new(&settings.asset_root))
        .layer(PropagateRequestIdLayer::new(request_id_header.clone()))
        .layer(SetRequestIdLayer::new(request_id_header, MakeRequestUuid))
        .layer(TraceLayer::new_for_http())
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(settings.api_addr).await?;
    info!(address = %settings.api_addr, "tktkgo API 已启动");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
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

async fn health() -> Json<serde_json::Value> {
    Json(json!({"status": "ok", "service": "api"}))
}

#[instrument(skip(state, request), fields(title = %request.title))]
async fn create_project(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<CreateProjectRequest>,
) -> Result<(StatusCode, Json<Project>), ApiError> {
    let auto_start = request.auto_start;
    let project = state.repository.create_project(&request).await?;
    if auto_start {
        dispatch_generation(&state, project.id).await?;
    }
    let project = state.repository.get_project(project.id).await?;
    Ok((StatusCode::CREATED, Json(project)))
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
    if !matches!(project.status.as_str(), "draft" | "failed" | "completed") {
        return Err(AppError::Conflict(format!("项目 {} 正在处理中", project.status)).into());
    }
    let workflow_id = Uuid::new_v4();
    state
        .repository
        .queue_project(project_id, workflow_id)
        .await?;
    let url = format!(
        "{}/VideoGenerationWorkflow/{}/run/send",
        state.settings.restate_ingress_url, workflow_id
    );
    let response = state
        .http
        .post(url)
        .header("idempotency-key", workflow_id.to_string())
        .json(&WorkflowInput { project_id })
        .send()
        .await
        .map_err(|err| AppError::external("restate", err.to_string()))?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        state
            .repository
            .update_project_status(project_id, "failed", Some("无法提交到 Restate"))
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
