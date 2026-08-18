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
use tracing::{info, instrument, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use uuid::Uuid;

#[derive(Clone)]
struct ApiState {
    repository: Repository,
    http: Client,
    provider_http: Client,
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
        provider_http: Client::builder()
            .timeout(std::time::Duration::from_secs(2))
            .build()?,
        settings: settings.clone(),
    });

    let request_id_header = HeaderName::from_static("x-request-id");
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

#[derive(Serialize)]
struct ProviderCatalog {
    speech: Vec<ProviderOption>,
    transcription: Vec<ProviderOption>,
}

#[derive(Serialize)]
struct ProviderOption {
    id: &'static str,
    label: &'static str,
    model: String,
    available: bool,
    voices: Vec<String>,
}

/// 返回当前部署实际可用的 Provider。前端只负责选择，不接触 API Key、服务地址或本地路径。
async fn list_providers(State(state): State<Arc<ApiState>>) -> Json<ProviderCatalog> {
    let (cosyvoice, faster_whisper) = tokio::join!(
        probe_provider(
            &state.provider_http,
            &state.settings.cosyvoice_base_url,
            "cosyvoice"
        ),
        probe_provider(
            &state.provider_http,
            &state.settings.faster_whisper_base_url,
            "faster-whisper"
        )
    );
    let cosyvoice_voices: Vec<String> = cosyvoice
        .as_ref()
        .and_then(|health| health.get("voices"))
        .and_then(serde_json::Value::as_array)
        .map(|voices| {
            voices
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let cosyvoice_model = health_model(cosyvoice.as_ref()).unwrap_or_default();
    let faster_whisper_model = health_model(faster_whisper.as_ref()).unwrap_or_default();
    let cosyvoice_available = !cosyvoice_model.is_empty() && !cosyvoice_voices.is_empty();
    let faster_whisper_available = !faster_whisper_model.is_empty();
    Json(ProviderCatalog {
        speech: vec![
            ProviderOption {
                id: "openai",
                label: "OpenAI",
                model: state.settings.tts_model.clone(),
                available: true,
                voices: vec!["coral".into(), "alloy".into(), "sage".into()],
            },
            ProviderOption {
                id: "cosyvoice",
                label: "CosyVoice 3（本地）",
                model: cosyvoice_model,
                available: cosyvoice_available,
                voices: cosyvoice_voices,
            },
        ],
        transcription: vec![
            ProviderOption {
                id: "openai",
                label: "OpenAI",
                model: state.settings.transcribe_model.clone(),
                available: true,
                voices: Vec::new(),
            },
            ProviderOption {
                id: "faster-whisper",
                label: "faster-whisper（本地）",
                model: faster_whisper_model,
                available: faster_whisper_available,
                voices: Vec::new(),
            },
        ],
    })
}

fn health_model(health: Option<&serde_json::Value>) -> Option<String> {
    health?
        .get("model")?
        .as_str()
        .filter(|model| !model.is_empty())
        .map(str::to_owned)
}

async fn probe_provider(
    client: &Client,
    base_url: &str,
    provider: &'static str,
) -> Option<serde_json::Value> {
    let result = async {
        let response = client.get(format!("{base_url}/health")).send().await?;
        response
            .error_for_status()?
            .json::<serde_json::Value>()
            .await
    }
    .await;
    match result {
        Ok(health)
            if health.get("status").and_then(serde_json::Value::as_str) == Some("ok")
                && health.get("provider").and_then(serde_json::Value::as_str) == Some(provider) =>
        {
            Some(health)
        }
        Ok(health) => {
            warn!(provider, response = %health, "本地 Provider 健康响应不符合协议");
            None
        }
        Err(error) => {
            warn!(provider, %error, "本地 Provider 当前不可用");
            None
        }
    }
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
        dispatch_generation(&state, project.id).await?;
    }
    let project = state.repository.get_project(project.id).await?;
    Ok((StatusCode::CREATED, Json(project)))
}

async fn ensure_requested_providers_available(
    state: &ApiState,
    request: &CreateProjectRequest,
) -> Result<(), ApiError> {
    match request.speech_provider.as_str() {
        "openai" if request.speech_model != state.settings.tts_model => {
            return Err(AppError::Validation(format!(
                "OpenAI 口播模型必须是 {}",
                state.settings.tts_model
            ))
            .into());
        }
        "cosyvoice" => {
            let health = probe_provider(
                &state.provider_http,
                &state.settings.cosyvoice_base_url,
                "cosyvoice",
            )
            .await
            .ok_or_else(|| AppError::Conflict("CosyVoice Provider 当前不可用".into()))?;
            ensure_model_matches("CosyVoice", &request.speech_model, &health)?;
        }
        _ => {}
    }
    match request.transcription_provider.as_str() {
        "openai" if request.transcription_model != state.settings.transcribe_model => {
            return Err(AppError::Validation(format!(
                "OpenAI 字幕模型必须是 {}",
                state.settings.transcribe_model
            ))
            .into());
        }
        "faster-whisper" => {
            let health = probe_provider(
                &state.provider_http,
                &state.settings.faster_whisper_base_url,
                "faster-whisper",
            )
            .await
            .ok_or_else(|| AppError::Conflict("faster-whisper Provider 当前不可用".into()))?;
            ensure_model_matches("faster-whisper", &request.transcription_model, &health)?;
        }
        _ => {}
    }
    Ok(())
}

fn ensure_model_matches(
    provider: &str,
    requested_model: &str,
    health: &serde_json::Value,
) -> Result<(), ApiError> {
    let running_model = health_model(Some(health))
        .ok_or_else(|| AppError::Conflict(format!("{provider} 未报告当前模型")))?;
    if requested_model != running_model {
        return Err(AppError::Conflict(format!(
            "{provider} 当前运行模型为 {running_model}，请求模型为 {requested_model}"
        ))
        .into());
    }
    Ok(())
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
