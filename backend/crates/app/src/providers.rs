use std::time::Instant;

use reqwest::{Client, multipart};
use schemars::schema_for;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::json;
use tracing::instrument;
use uuid::Uuid;

use crate::{
    AppError, AppResult, Settings,
    domain::{CaptionCue, Project, ScriptSpec, StoryboardSpec},
};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProviderUsage {
    pub request_id: Option<String>,
    pub input_units: Option<i64>,
    pub output_units: Option<i64>,
    pub latency_ms: i64,
}

#[derive(Clone, Debug)]
pub struct ProviderOutput<T> {
    pub value: T,
    pub usage: ProviderUsage,
}

/// Model Gateway 暴露给业务服务的唯一能力目录结构。
/// API 直接透传该类型，避免业务 API 再维护一份手写协议。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderCatalog {
    pub text: Vec<ProviderOption>,
    pub image: Vec<ProviderOption>,
    pub speech: Vec<ProviderOption>,
    pub alignment: Vec<ProviderOption>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderOption {
    pub id: String,
    pub label: String,
    pub model: String,
    pub available: bool,
    pub voices: Vec<String>,
}

/// Rust Pipeline 使用的固定图片能力请求；供应商尺寸枚举和裁剪策略不进入业务层。
pub struct ImageGenerationRequest<'a> {
    pub prompt: &'a str,
    pub width: i32,
    pub height: i32,
    pub quality: &'a str,
    pub provider: &'a str,
    pub model: &'a str,
    pub request_id: Uuid,
}

/// 业务服务中唯一的模型 Provider 实现，只理解稳定的 Model Gateway HTTP 协议。
#[derive(Clone)]
pub struct ModelGatewayClient {
    client: Client,
    base_url: String,
}

impl ModelGatewayClient {
    pub fn new(settings: &Settings) -> AppResult<Self> {
        let client = Client::builder()
            // 单个图片或长口播生成可能耗时较长，重试仍由 Restate 外部步骤负责。
            .timeout(std::time::Duration::from_secs(30 * 60))
            .build()
            .map_err(|error| AppError::external("model-gateway", error.to_string()))?;
        Ok(Self {
            client,
            base_url: settings.model_gateway_url.clone(),
        })
    }

    /// 获取当前真正可用的 Provider/模型组合。可用性判断由适配层负责。
    #[instrument(skip(self))]
    pub async fn list_models(&self) -> AppResult<ProviderCatalog> {
        let response = self
            .client
            .get(format!("{}/v1/models", self.base_url))
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .map_err(gateway_error)?;
        let body = checked_bytes(response).await?;
        serde_json::from_slice(&body).map_err(|error| {
            AppError::external("model-gateway", format!("模型目录解析失败: {error}"))
        })
    }

    async fn structured<T: DeserializeOwned + schemars::JsonSchema>(
        &self,
        provider: &str,
        model: &str,
        system: &str,
        prompt: String,
        schema_name: &str,
        request_id: Uuid,
    ) -> AppResult<ProviderOutput<T>> {
        let response = self
            .client
            .post(format!("{}/v1/text/generate", self.base_url))
            .json(&json!({
                "provider": provider,
                "model": model,
                "system": system,
                "prompt": prompt,
                "schema_name": schema_name,
                "output_schema": schema_for!(T),
                "request_id": request_id,
            }))
            .send()
            .await
            .map_err(gateway_error)?;
        let body = checked_bytes(response).await?;
        let output: GatewayTextOutput<T> = serde_json::from_slice(&body).map_err(|error| {
            AppError::external("model-gateway", format!("结构化响应解析失败: {error}"))
        })?;
        Ok(ProviderOutput {
            value: output.output,
            usage: output.usage,
        })
    }
}

#[derive(Deserialize)]
struct GatewayTextOutput<T> {
    output: T,
    usage: ProviderUsage,
}

#[derive(Deserialize)]
struct GatewayAlignmentOutput {
    cues: Vec<CaptionCue>,
    usage: ProviderUsage,
}

impl ModelGatewayClient {
    #[instrument(skip(self, project), fields(project_id = %project.id, provider, model, request_id = %request_id))]
    pub async fn generate_script(
        &self,
        project: &Project,
        provider: &str,
        model: &str,
        request_id: Uuid,
    ) -> AppResult<ProviderOutput<ScriptSpec>> {
        let system = "你是资深视频策划和中文口播编辑。输出可直接口播的自然文稿，事实不确定时不要编造。严格遵守 JSON Schema，不输出 Markdown。";
        let prompt = format!(
            "根据下面主题或大纲生成完整视频文稿。语言：{}；目标时长：{}秒；画幅：{}。文稿需要有开场钩子、清晰结构、自然转场和结尾总结。视觉风格必须具体且全片一致。\n\n标题：{}\n输入：{}",
            project.language,
            project.target_duration_seconds,
            project.aspect_ratio,
            project.title,
            project.source_text
        );
        self.structured(provider, model, system, prompt, "video_script", request_id)
            .await
    }

    #[instrument(skip(self, project, script), fields(project_id = %project.id, provider, model, request_id = %request_id))]
    pub async fn generate_storyboard(
        &self,
        project: &Project,
        script: &ScriptSpec,
        provider: &str,
        model: &str,
        request_id: Uuid,
    ) -> AppResult<ProviderOutput<StoryboardSpec>> {
        let system = "你是视频导演和分镜设计师。把口播稿拆成可渲染场景。每个场景约3到10秒，narration 合并后不得遗漏或改写原文。插图提示词要包含构图、主体、光线、色彩和统一风格，不要在图片中生成文字。严格遵守 JSON Schema。";
        let prompt = format!(
            "画幅：{}；视觉方向：{}。请为以下文稿设计分镜：\n{}",
            project.aspect_ratio, script.visual_style, script.full_narration
        );
        self.structured(
            provider,
            model,
            system,
            prompt,
            "video_storyboard",
            request_id,
        )
        .await
    }
}

impl ModelGatewayClient {
    #[instrument(
        skip(self, request),
        fields(
            provider = request.provider,
            model = request.model,
            width = request.width,
            height = request.height,
            quality = request.quality,
            request_id = %request.request_id
        )
    )]
    pub async fn generate_image(
        &self,
        request: ImageGenerationRequest<'_>,
    ) -> AppResult<ProviderOutput<Vec<u8>>> {
        let ImageGenerationRequest {
            prompt,
            width,
            height,
            quality,
            provider,
            model,
            request_id,
        } = request;
        let started = Instant::now();
        let response = self
            .client
            .post(format!("{}/v1/images/generate", self.base_url))
            .json(&json!({
                "provider": provider,
                "model": model,
                "prompt": prompt,
                "width": width,
                "height": height,
                "quality": quality,
                "request_id": request_id,
            }))
            .send()
            .await
            .map_err(gateway_error)?;
        binary_output(response, started, Some(prompt.chars().count() as i64)).await
    }
}

impl ModelGatewayClient {
    #[instrument(skip(self, text), fields(provider, model, voice, request_id = %request_id, text_chars = text.chars().count()))]
    pub async fn synthesize_speech(
        &self,
        text: &str,
        voice: &str,
        provider: &str,
        model: &str,
        request_id: Uuid,
    ) -> AppResult<ProviderOutput<Vec<u8>>> {
        let started = Instant::now();
        let response = self
            .client
            .post(format!("{}/v1/audio/speech", self.base_url))
            .json(&json!({
                "provider": provider,
                "model": model,
                "text": text,
                "voice": voice,
                "output_format": "wav",
                "instructions": "自然、清晰、有亲和力的专业中文口播，停顿适中。",
                "request_id": request_id,
            }))
            .send()
            .await
            .map_err(gateway_error)?;
        binary_output(response, started, Some(text.chars().count() as i64)).await
    }
}

impl ModelGatewayClient {
    #[instrument(skip(self, audio, canonical_text), fields(provider, model, request_id = %request_id, audio_bytes = audio.len()))]
    pub async fn align_speech(
        &self,
        audio: Vec<u8>,
        canonical_text: &str,
        provider: &str,
        model: &str,
        request_id: Uuid,
    ) -> AppResult<ProviderOutput<Vec<CaptionCue>>> {
        let audio = multipart::Part::bytes(audio)
            .file_name("speech.wav")
            .mime_str("audio/wav")
            .map_err(gateway_error)?;
        let form = multipart::Form::new()
            .text("provider", provider.to_owned())
            .text("model", model.to_owned())
            .text("canonical_text", canonical_text.to_owned())
            .text("request_id", request_id.to_string())
            .part("file", audio);
        let response = self
            .client
            .post(format!("{}/v1/audio/align", self.base_url))
            .multipart(form)
            .send()
            .await
            .map_err(gateway_error)?;
        let body = checked_bytes(response).await?;
        let output: GatewayAlignmentOutput = serde_json::from_slice(&body).map_err(|error| {
            AppError::external("model-gateway", format!("字幕对齐响应解析失败: {error}"))
        })?;
        if output.cues.is_empty() {
            return Err(AppError::external(
                "model-gateway",
                "字幕对齐响应没有字符级时间戳",
            ));
        }
        Ok(ProviderOutput {
            value: output.cues,
            usage: output.usage,
        })
    }
}

async fn binary_output(
    response: reqwest::Response,
    started: Instant,
    input_units: Option<i64>,
) -> AppResult<ProviderOutput<Vec<u8>>> {
    let request_id = response
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let latency_ms = response
        .headers()
        .get("x-latency-ms")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| started.elapsed().as_millis() as i64);
    let value = checked_bytes(response).await?;
    Ok(ProviderOutput {
        value,
        usage: ProviderUsage {
            request_id,
            input_units,
            latency_ms,
            ..Default::default()
        },
    })
}

async fn checked_bytes(response: reqwest::Response) -> AppResult<Vec<u8>> {
    let status = response.status();
    let body = response.bytes().await.map_err(gateway_error)?;
    if !status.is_success() {
        return Err(AppError::external(
            "model-gateway",
            format!("HTTP {status}: {}", String::from_utf8_lossy(&body)),
        ));
    }
    Ok(body.to_vec())
}

fn gateway_error(error: impl std::fmt::Display) -> AppError {
    AppError::external("model-gateway", error.to_string())
}
