use std::{sync::Arc, time::Instant};

use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::STANDARD};
use reqwest::{Client, StatusCode, multipart};
use schemars::schema_for;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use tracing::{instrument, warn};
use uuid::Uuid;

use crate::{
    AppError, AppResult, Settings,
    domain::{CaptionCue, Project, ScriptSpec, StoryboardSpec},
};

mod local;

pub use local::{CosyVoiceSpeechProvider, FasterWhisperTranscriptionProvider};

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

/// 所有生成 Provider 的公共身份信息。
///
/// 能力接口不再绑定到同一个供应商：文本、图片、口播和转写可以分别注入不同实现。
pub trait ProviderIdentity: Send + Sync {
    fn name(&self) -> &'static str;
}

#[async_trait]
/// 生成强类型文稿和分镜的文本能力。
pub trait TextProvider: ProviderIdentity {
    fn model(&self) -> &str;
    async fn generate_script(
        &self,
        project: &Project,
        request_id: Uuid,
    ) -> AppResult<ProviderOutput<ScriptSpec>>;
    async fn generate_storyboard(
        &self,
        project: &Project,
        script: &ScriptSpec,
        request_id: Uuid,
    ) -> AppResult<ProviderOutput<StoryboardSpec>>;
}

#[async_trait]
/// 生成场景插图的图片能力；当前 Pipeline 约定返回 PNG 字节。
pub trait ImageProvider: ProviderIdentity {
    fn model(&self) -> &str;
    async fn generate_image(
        &self,
        prompt: &str,
        portrait: bool,
        request_id: Uuid,
    ) -> AppResult<ProviderOutput<Vec<u8>>>;
}

#[async_trait]
/// 将场景口播文本合成为音频；当前 Pipeline 约定返回可被 ffprobe 读取的 WAV 字节。
pub trait SpeechProvider: ProviderIdentity {
    async fn synthesize_speech(
        &self,
        text: &str,
        voice: &str,
        model: &str,
        request_id: Uuid,
    ) -> AppResult<ProviderOutput<Vec<u8>>>;
}

#[async_trait]
/// 根据已生成的口播音频返回词级时间戳，用于构建确定性字幕时间线。
pub trait TranscriptionProvider: ProviderIdentity {
    async fn align_speech(
        &self,
        audio: Vec<u8>,
        canonical_text: &str,
        model: &str,
        request_id: Uuid,
    ) -> AppResult<ProviderOutput<Vec<CaptionCue>>>;
}

/// Pipeline 使用的 Provider 集合。
///
/// 该结构是编排层唯一依赖的能力入口。替换某一种能力时，不需要修改工作流和其他 Provider。
#[derive(Clone)]
pub struct GenerationProviders {
    text: Arc<dyn TextProvider>,
    image: Arc<dyn ImageProvider>,
    openai_speech: Arc<dyn SpeechProvider>,
    cosyvoice_speech: Arc<dyn SpeechProvider>,
    openai_transcription: Arc<dyn TranscriptionProvider>,
    faster_whisper_transcription: Arc<dyn TranscriptionProvider>,
}

impl GenerationProviders {
    pub fn new(
        text: Arc<dyn TextProvider>,
        image: Arc<dyn ImageProvider>,
        openai_speech: Arc<dyn SpeechProvider>,
        cosyvoice_speech: Arc<dyn SpeechProvider>,
        openai_transcription: Arc<dyn TranscriptionProvider>,
        faster_whisper_transcription: Arc<dyn TranscriptionProvider>,
    ) -> Self {
        Self {
            text,
            image,
            openai_speech,
            cosyvoice_speech,
            openai_transcription,
            faster_whisper_transcription,
        }
    }

    pub fn text(&self) -> &dyn TextProvider {
        self.text.as_ref()
    }

    pub fn image(&self) -> &dyn ImageProvider {
        self.image.as_ref()
    }

    /// 根据项目保存的选择解析口播实现。未知值必须直接失败，避免静默换用其他供应商。
    pub fn speech(&self, provider: &str) -> AppResult<&dyn SpeechProvider> {
        match provider {
            "openai" => Ok(self.openai_speech.as_ref()),
            "cosyvoice" => Ok(self.cosyvoice_speech.as_ref()),
            value => Err(AppError::Config(format!(
                "项目配置了不支持的口播 Provider: {value}"
            ))),
        }
    }

    /// 根据项目保存的选择解析字幕实现。
    pub fn transcription(&self, provider: &str) -> AppResult<&dyn TranscriptionProvider> {
        match provider {
            "openai" => Ok(self.openai_transcription.as_ref()),
            "faster-whisper" => Ok(self.faster_whisper_transcription.as_ref()),
            value => Err(AppError::Config(format!(
                "项目配置了不支持的字幕 Provider: {value}"
            ))),
        }
    }
}

#[derive(Clone)]
pub struct OpenAiProvider {
    client: Client,
    base_url: String,
    api_key: String,
    text_model: String,
    image_model: String,
}

impl OpenAiProvider {
    pub fn new(settings: &Settings) -> AppResult<Self> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .map_err(|err| AppError::external("openai", err.to_string()))?;
        Ok(Self {
            client,
            base_url: settings.openai_base_url.clone(),
            api_key: settings.openai_api_key.clone(),
            text_model: settings.text_model.clone(),
            image_model: settings.image_model.clone(),
        })
    }

    async fn structured<T: DeserializeOwned + schemars::JsonSchema>(
        &self,
        system: &str,
        user: String,
        schema_name: &str,
        request_id: Uuid,
    ) -> AppResult<ProviderOutput<T>> {
        let started = Instant::now();
        let schema = serde_json::to_value(schema_for!(T))?;
        let payload = json!({
            "model": self.text_model,
            "input": [
                {"role": "system", "content": system},
                {"role": "user", "content": user}
            ],
            "text": {"format": {"type": "json_schema", "name": schema_name, "strict": true, "schema": schema}},
            "reasoning": {"effort": "medium"}
        });
        let response = self
            .client
            .post(format!("{}/responses", self.base_url))
            .bearer_auth(&self.api_key)
            .header("X-Client-Request-Id", request_id.to_string())
            .json(&payload)
            .send()
            .await
            .map_err(|err| AppError::external("openai", err.to_string()))?;
        let request_header = response
            .headers()
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let value = checked_json(response).await?;
        let text = extract_output_text(&value)
            .ok_or_else(|| AppError::external("openai", "Responses API 未返回 output_text"))?;
        let parsed = serde_json::from_str(text).map_err(|err| {
            AppError::external(
                "openai",
                format!("结构化输出解析失败: {err}; output={text}"),
            )
        })?;
        let usage = ProviderUsage {
            request_id: request_header,
            input_units: value.pointer("/usage/input_tokens").and_then(Value::as_i64),
            output_units: value
                .pointer("/usage/output_tokens")
                .and_then(Value::as_i64),
            latency_ms: started.elapsed().as_millis() as i64,
        };
        Ok(ProviderOutput {
            value: parsed,
            usage,
        })
    }
}

impl ProviderIdentity for OpenAiProvider {
    fn name(&self) -> &'static str {
        "openai"
    }
}

#[async_trait]
impl TextProvider for OpenAiProvider {
    fn model(&self) -> &str {
        &self.text_model
    }

    #[instrument(skip(self, project), fields(project_id = %project.id, model = %self.text_model))]
    async fn generate_script(
        &self,
        project: &Project,
        request_id: Uuid,
    ) -> AppResult<ProviderOutput<ScriptSpec>> {
        let system = "你是资深视频策划和中文口播编辑。输出可直接口播的自然文稿，事实不确定时不要编造。严格遵守 JSON Schema，不输出 Markdown。";
        let user = format!(
            "根据下面主题或大纲生成完整视频文稿。语言：{}；目标时长：{}秒；画幅：{}。文稿需要有开场钩子、清晰结构、自然转场和结尾总结。视觉风格必须具体且全片一致。\n\n标题：{}\n输入：{}",
            project.language,
            project.target_duration_seconds,
            project.aspect_ratio,
            project.title,
            project.source_text
        );
        self.structured(system, user, "video_script", request_id)
            .await
    }

    #[instrument(skip(self, project, script), fields(project_id = %project.id, model = %self.text_model))]
    async fn generate_storyboard(
        &self,
        project: &Project,
        script: &ScriptSpec,
        request_id: Uuid,
    ) -> AppResult<ProviderOutput<StoryboardSpec>> {
        let system = "你是视频导演和分镜设计师。把口播稿拆成可渲染场景。每个场景约3到10秒，narration 合并后不得遗漏或改写原文。插图提示词要包含构图、主体、光线、色彩和统一风格，不要在图片中生成文字。严格遵守 JSON Schema。";
        let user = format!(
            "画幅：{}；视觉方向：{}。请为以下文稿设计分镜：\n{}",
            project.aspect_ratio, script.visual_style, script.full_narration
        );
        self.structured(system, user, "video_storyboard", request_id)
            .await
    }
}

#[async_trait]
impl ImageProvider for OpenAiProvider {
    fn model(&self) -> &str {
        &self.image_model
    }

    #[instrument(skip(self, prompt), fields(model = %self.image_model, portrait, request_id = %request_id))]
    async fn generate_image(
        &self,
        prompt: &str,
        portrait: bool,
        request_id: Uuid,
    ) -> AppResult<ProviderOutput<Vec<u8>>> {
        let started = Instant::now();
        let size = if portrait { "1024x1536" } else { "1536x1024" };
        let response = self.client.post(format!("{}/images/generations", self.base_url))
            .bearer_auth(&self.api_key).header("X-Client-Request-Id", request_id.to_string())
            .json(&json!({"model": self.image_model, "prompt": prompt, "size": size, "quality": "medium", "output_format": "png"}))
            .send().await.map_err(|err| AppError::external("openai", err.to_string()))?;
        let provider_request_id = response
            .headers()
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let value = checked_json(response).await?;
        let bytes = if let Some(encoded) = value.pointer("/data/0/b64_json").and_then(Value::as_str)
        {
            STANDARD.decode(encoded).map_err(|err| {
                AppError::external("openai", format!("图片 Base64 解码失败: {err}"))
            })?
        } else if let Some(url) = value.pointer("/data/0/url").and_then(Value::as_str) {
            self.client
                .get(url)
                .send()
                .await
                .map_err(|err| AppError::external("openai-image", err.to_string()))?
                .error_for_status()
                .map_err(|err| AppError::external("openai-image", err.to_string()))?
                .bytes()
                .await
                .map_err(|err| AppError::external("openai-image", err.to_string()))?
                .to_vec()
        } else {
            return Err(AppError::external(
                "openai",
                "图片接口未返回 b64_json 或 url",
            ));
        };
        Ok(ProviderOutput {
            value: bytes,
            usage: ProviderUsage {
                request_id: provider_request_id,
                latency_ms: started.elapsed().as_millis() as i64,
                ..Default::default()
            },
        })
    }
}

#[async_trait]
impl SpeechProvider for OpenAiProvider {
    #[instrument(skip(self, text), fields(model, request_id = %request_id, text_chars = text.chars().count()))]
    async fn synthesize_speech(
        &self,
        text: &str,
        voice: &str,
        model: &str,
        request_id: Uuid,
    ) -> AppResult<ProviderOutput<Vec<u8>>> {
        let started = Instant::now();
        let response = self.client.post(format!("{}/audio/speech", self.base_url))
            .bearer_auth(&self.api_key).header("X-Client-Request-Id", request_id.to_string())
            .json(&json!({"model": model, "voice": voice, "input": text, "response_format": "wav", "instructions": "自然、清晰、有亲和力的专业中文口播，停顿适中。"}))
            .send().await.map_err(|err| AppError::external("openai", err.to_string()))?;
        let provider_request_id = response
            .headers()
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|err| AppError::external("openai", err.to_string()))?;
        if !status.is_success() {
            return Err(AppError::external(
                "openai",
                String::from_utf8_lossy(&bytes).into_owned(),
            ));
        }
        Ok(ProviderOutput {
            value: bytes.to_vec(),
            usage: ProviderUsage {
                request_id: provider_request_id,
                input_units: Some(text.chars().count() as i64),
                latency_ms: started.elapsed().as_millis() as i64,
                ..Default::default()
            },
        })
    }
}

#[async_trait]
impl TranscriptionProvider for OpenAiProvider {
    #[instrument(skip(self, audio, canonical_text), fields(model, request_id = %request_id, audio_bytes = audio.len()))]
    async fn align_speech(
        &self,
        audio: Vec<u8>,
        canonical_text: &str,
        model: &str,
        request_id: Uuid,
    ) -> AppResult<ProviderOutput<Vec<CaptionCue>>> {
        let started = Instant::now();
        let part = multipart::Part::bytes(audio)
            .file_name("speech.wav")
            .mime_str("audio/wav")
            .map_err(|err| AppError::external("openai", err.to_string()))?;
        let form = multipart::Form::new()
            .text("model", model.to_owned())
            .text("response_format", "verbose_json")
            .text("timestamp_granularities[]", "word")
            .text("prompt", canonical_text.to_owned())
            .part("file", part);
        let response = self
            .client
            .post(format!("{}/audio/transcriptions", self.base_url))
            .bearer_auth(&self.api_key)
            .header("X-Client-Request-Id", request_id.to_string())
            .multipart(form)
            .send()
            .await
            .map_err(|err| AppError::external("openai", err.to_string()))?;
        let provider_request_id = response
            .headers()
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let value = checked_json(response).await?;
        let words = value
            .get("words")
            .and_then(Value::as_array)
            .ok_or_else(|| AppError::external("openai", "转写结果缺少 words 时间戳"))?;
        let cues = words
            .iter()
            .filter_map(|word| {
                Some(CaptionCue {
                    text: word.get("word")?.as_str()?.to_owned(),
                    start_ms: (word.get("start")?.as_f64()? * 1000.0).round() as i64,
                    end_ms: (word.get("end")?.as_f64()? * 1000.0).round() as i64,
                })
            })
            .collect();
        Ok(ProviderOutput {
            value: cues,
            usage: ProviderUsage {
                request_id: provider_request_id,
                latency_ms: started.elapsed().as_millis() as i64,
                ..Default::default()
            },
        })
    }
}

async fn checked_json(response: reqwest::Response) -> AppResult<Value> {
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|err| AppError::external("openai", err.to_string()))?;
    if status == StatusCode::TOO_MANY_REQUESTS {
        warn!(%status, "OpenAI 触发速率限制，将由工作流重试");
    }
    if !status.is_success() {
        return Err(AppError::external(
            "openai",
            format!("HTTP {status}: {body}"),
        ));
    }
    serde_json::from_str(&body).map_err(Into::into)
}

fn extract_output_text(value: &Value) -> Option<&str> {
    value
        .get("output")?
        .as_array()?
        .iter()
        .flat_map(|item| {
            item.get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .find(|content| content.get("type").and_then(Value::as_str) == Some("output_text"))?
        .get("text")?
        .as_str()
}
