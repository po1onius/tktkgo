use std::time::Instant;

use async_trait::async_trait;
use reqwest::{Client, multipart};
use serde::{Deserialize, Serialize};
use tracing::instrument;
use uuid::Uuid;

use crate::{
    AppError, AppResult, Settings,
    domain::CaptionCue,
    providers::{
        ProviderIdentity, ProviderOutput, ProviderUsage, SpeechProvider, TranscriptionProvider,
    },
};

#[derive(Clone)]
pub struct CosyVoiceSpeechProvider {
    client: Client,
    base_url: String,
}

impl CosyVoiceSpeechProvider {
    pub fn new(settings: &Settings) -> AppResult<Self> {
        Ok(Self {
            client: local_client("cosyvoice")?,
            base_url: settings.cosyvoice_base_url.clone(),
        })
    }
}

impl ProviderIdentity for CosyVoiceSpeechProvider {
    fn name(&self) -> &'static str {
        "cosyvoice"
    }
}

#[derive(Serialize)]
struct CosyVoiceRequest<'a> {
    model: &'a str,
    voice: &'a str,
    input: &'a str,
    instructions: &'a str,
    request_id: Uuid,
}

#[async_trait]
impl SpeechProvider for CosyVoiceSpeechProvider {
    #[instrument(skip(self, text), fields(provider = self.name(), model, request_id = %request_id, text_chars = text.chars().count(), voice))]
    async fn synthesize_speech(
        &self,
        text: &str,
        voice: &str,
        model: &str,
        request_id: Uuid,
    ) -> AppResult<ProviderOutput<Vec<u8>>> {
        let started = Instant::now();
        let response = self
            .client
            .post(format!("{}/v1/speech", self.base_url))
            .json(&CosyVoiceRequest {
                model,
                voice,
                input: text,
                instructions: "自然、清晰、有亲和力的专业中文口播，停顿适中。",
                request_id,
            })
            .send()
            .await
            .map_err(|err| AppError::external(self.name(), err.to_string()))?;
        let bytes = checked_bytes(self.name(), response).await?;
        Ok(ProviderOutput {
            value: bytes,
            usage: ProviderUsage {
                request_id: Some(request_id.to_string()),
                input_units: Some(text.chars().count() as i64),
                latency_ms: started.elapsed().as_millis() as i64,
                ..Default::default()
            },
        })
    }
}

#[derive(Clone)]
pub struct FasterWhisperTranscriptionProvider {
    client: Client,
    base_url: String,
}

impl FasterWhisperTranscriptionProvider {
    pub fn new(settings: &Settings) -> AppResult<Self> {
        Ok(Self {
            client: local_client("faster-whisper")?,
            base_url: settings.faster_whisper_base_url.clone(),
        })
    }
}

impl ProviderIdentity for FasterWhisperTranscriptionProvider {
    fn name(&self) -> &'static str {
        "faster-whisper"
    }
}

#[derive(Deserialize)]
struct AlignmentResponse {
    cues: Vec<CaptionCue>,
}

#[async_trait]
impl TranscriptionProvider for FasterWhisperTranscriptionProvider {
    #[instrument(skip(self, audio, canonical_text), fields(provider = self.name(), model, request_id = %request_id, audio_bytes = audio.len(), text_chars = canonical_text.chars().count()))]
    async fn align_speech(
        &self,
        audio: Vec<u8>,
        canonical_text: &str,
        model: &str,
        request_id: Uuid,
    ) -> AppResult<ProviderOutput<Vec<CaptionCue>>> {
        let started = Instant::now();
        let audio = multipart::Part::bytes(audio)
            .file_name("speech.wav")
            .mime_str("audio/wav")
            .map_err(|err| AppError::external(self.name(), err.to_string()))?;
        let form = multipart::Form::new()
            .text("model", model.to_owned())
            .text("canonical_text", canonical_text.to_owned())
            .text("request_id", request_id.to_string())
            .part("file", audio);
        let response = self
            .client
            .post(format!("{}/v1/align", self.base_url))
            .multipart(form)
            .send()
            .await
            .map_err(|err| AppError::external(self.name(), err.to_string()))?;
        let body = checked_bytes(self.name(), response).await?;
        let result: AlignmentResponse = serde_json::from_slice(&body)
            .map_err(|err| AppError::external(self.name(), format!("无法解析字幕响应: {err}")))?;
        if result.cues.is_empty() {
            return Err(AppError::external(self.name(), "字幕响应没有词级时间戳"));
        }
        Ok(ProviderOutput {
            value: result.cues,
            usage: ProviderUsage {
                request_id: Some(request_id.to_string()),
                latency_ms: started.elapsed().as_millis() as i64,
                ..Default::default()
            },
        })
    }
}

fn local_client(provider: &'static str) -> AppResult<Client> {
    Client::builder()
        // 本地模型首次推理和长口播可能耗时较长，超时由工作流外部步骤统一重试。
        .timeout(std::time::Duration::from_secs(30 * 60))
        .build()
        .map_err(|err| AppError::external(provider, err.to_string()))
}

async fn checked_bytes(provider: &'static str, response: reqwest::Response) -> AppResult<Vec<u8>> {
    let status = response.status();
    let body = response
        .bytes()
        .await
        .map_err(|err| AppError::external(provider, err.to_string()))?;
    if !status.is_success() {
        return Err(AppError::external(
            provider,
            format!("HTTP {status}: {}", String::from_utf8_lossy(&body)),
        ));
    }
    Ok(body.to_vec())
}
