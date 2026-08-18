use chrono::{DateTime, Utc};
use diesel::{Insertable, Queryable, Selectable};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{AppError, AppResult, schema};

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AspectRatio {
    Landscape,
    Portrait,
    Square,
}

impl AspectRatio {
    pub fn database_value(&self) -> &'static str {
        match self {
            Self::Landscape => "16:9",
            Self::Portrait => "9:16",
            Self::Square => "1:1",
        }
    }

    pub fn dimensions(&self) -> (i32, i32) {
        match self {
            Self::Landscape => (1920, 1080),
            Self::Portrait => (1080, 1920),
            Self::Square => (1080, 1080),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CreateProjectRequest {
    pub title: String,
    pub source_text: String,
    #[serde(default = "default_language")]
    pub language: String,
    #[serde(default = "default_aspect_ratio")]
    pub aspect_ratio: AspectRatio,
    #[serde(default = "default_duration")]
    pub target_duration_seconds: i32,
    #[serde(default = "default_voice")]
    pub voice: String,
    #[serde(default = "default_speech_provider")]
    pub speech_provider: String,
    #[serde(default = "default_speech_model")]
    pub speech_model: String,
    #[serde(default = "default_transcription_provider")]
    pub transcription_provider: String,
    #[serde(default = "default_transcription_model")]
    pub transcription_model: String,
    #[serde(default = "default_review")]
    pub require_script_review: bool,
    #[serde(default = "default_auto_start")]
    pub auto_start: bool,
}

impl CreateProjectRequest {
    pub fn validate(&self) -> AppResult<()> {
        if self.title.trim().is_empty() || self.title.chars().count() > 200 {
            return Err(AppError::Validation("标题必须为 1 到 200 个字符".into()));
        }
        if self.source_text.trim().is_empty() || self.source_text.chars().count() > 20_000 {
            return Err(AppError::Validation(
                "主题或大纲必须为 1 到 20000 个字符".into(),
            ));
        }
        if !(10..=3600).contains(&self.target_duration_seconds) {
            return Err(AppError::Validation(
                "目标时长必须在 10 到 3600 秒之间".into(),
            ));
        }
        if !matches!(self.speech_provider.as_str(), "openai" | "cosyvoice") {
            return Err(AppError::Validation(format!(
                "不支持的口播 Provider: {}",
                self.speech_provider
            )));
        }
        if !matches!(
            self.transcription_provider.as_str(),
            "openai" | "faster-whisper"
        ) {
            return Err(AppError::Validation(format!(
                "不支持的字幕 Provider: {}",
                self.transcription_provider
            )));
        }
        for (label, model) in [
            ("口播模型", self.speech_model.as_str()),
            ("字幕模型", self.transcription_model.as_str()),
        ] {
            if model.trim().is_empty() || model.chars().count() > 128 {
                return Err(AppError::Validation(format!(
                    "{label}必须为 1 到 128 个字符"
                )));
            }
        }
        Ok(())
    }
}

fn default_language() -> String {
    "zh-CN".into()
}
fn default_aspect_ratio() -> AspectRatio {
    AspectRatio::Landscape
}
fn default_duration() -> i32 {
    180
}
fn default_voice() -> String {
    "coral".into()
}
fn default_speech_provider() -> String {
    "openai".into()
}
fn default_speech_model() -> String {
    "gpt-4o-mini-tts".into()
}
fn default_transcription_provider() -> String {
    "openai".into()
}
fn default_transcription_model() -> String {
    "whisper-1".into()
}
fn default_review() -> bool {
    true
}
fn default_auto_start() -> bool {
    true
}

#[derive(Clone, Debug, Queryable, Selectable, Serialize, Deserialize)]
#[diesel(table_name = schema::projects)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Project {
    pub id: Uuid,
    pub title: String,
    pub source_text: String,
    pub language: String,
    pub aspect_ratio: String,
    pub target_duration_seconds: i32,
    pub voice: String,
    pub speech_provider: String,
    pub speech_model: String,
    pub transcription_provider: String,
    pub transcription_model: String,
    pub require_script_review: bool,
    pub status: String,
    pub active_workflow_id: Option<Uuid>,
    pub current_version: i32,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Insertable)]
#[diesel(table_name = schema::projects)]
pub struct NewProject<'a> {
    pub id: Uuid,
    pub title: &'a str,
    pub source_text: &'a str,
    pub language: &'a str,
    pub aspect_ratio: &'a str,
    pub target_duration_seconds: i32,
    pub voice: &'a str,
    pub speech_provider: &'a str,
    pub speech_model: &'a str,
    pub transcription_provider: &'a str,
    pub transcription_model: &'a str,
    pub require_script_review: bool,
}

#[derive(Clone, Debug, Queryable, Selectable, Serialize, Deserialize)]
#[diesel(table_name = schema::project_versions)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ProjectVersion {
    pub id: Uuid,
    pub project_id: Uuid,
    pub version: i32,
    pub script_spec: Option<serde_json::Value>,
    pub storyboard_spec: Option<serde_json::Value>,
    pub render_spec: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScriptSpec {
    pub title: String,
    pub audience: String,
    pub tone: String,
    pub visual_style: String,
    pub summary: String,
    pub sections: Vec<ScriptSection>,
    pub full_narration: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScriptSection {
    pub heading: String,
    pub purpose: String,
    pub narration: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StoryboardSpec {
    pub style_bible: StyleBible,
    pub scenes: Vec<SceneDraft>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StyleBible {
    pub art_direction: String,
    pub color_palette: Vec<String>,
    pub typography: String,
    pub image_rules: Vec<String>,
    pub negative_prompt: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SceneDraft {
    pub sequence: i32,
    pub narration: String,
    pub visual_type: VisualType,
    pub visual_prompt: String,
    #[schemars(required)]
    pub on_screen_text: Option<String>,
    pub transition: TransitionKind,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VisualType {
    Illustration,
    Infographic,
    Quote,
    Title,
    List,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TransitionKind {
    Fade,
    Slide,
    Wipe,
    None,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct CaptionCue {
    pub text: String,
    pub start_ms: i64,
    pub end_ms: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct RenderSpec {
    pub project_id: Uuid,
    pub version: i32,
    pub width: i32,
    pub height: i32,
    pub fps: i32,
    pub background_color: String,
    pub scenes: Vec<RenderScene>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct RenderScene {
    pub id: Uuid,
    pub sequence: i32,
    pub start_frame: i64,
    pub duration_in_frames: i64,
    pub narration: String,
    pub visual_type: VisualType,
    pub image_url: String,
    pub audio_url: String,
    pub on_screen_text: Option<String>,
    pub transition: TransitionKind,
    pub captions: Vec<CaptionCue>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkflowInput {
    pub project_id: Uuid,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScriptReviewInput {
    pub approved: bool,
    #[serde(default)]
    pub feedback: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkflowResult {
    pub project_id: Uuid,
    pub version: i32,
    pub outcome: String,
    pub render_url: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RenderJobRequest {
    pub render_id: Uuid,
    pub output_key: String,
    pub spec: RenderSpec,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RenderJobResult {
    pub storage_key: String,
    pub public_url: String,
    pub duration_ms: i64,
}
