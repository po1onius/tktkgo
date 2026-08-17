use std::{path::PathBuf, sync::Arc};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use tracing::{info, instrument};
use uuid::Uuid;

use crate::{
    AppError, AppResult,
    db::{AssetRecord, NewAsset, NewGenerationTask, NewRender, Repository, SceneRecord},
    domain::{
        CaptionCue, Project, ProjectVersion, RenderJobRequest, RenderScene, RenderSpec, ScriptSpec,
        StoryboardSpec, TransitionKind, VisualType,
    },
    media::probe_duration_ms,
    providers::{AiProvider, ProviderOutput},
    render::RenderClient,
    storage::AssetStore,
};

#[derive(Clone)]
pub struct PipelineService {
    repository: Repository,
    ai: Arc<dyn AiProvider>,
    storage: Arc<dyn AssetStore>,
    renderer: RenderClient,
    asset_root: PathBuf,
}

impl PipelineService {
    pub fn new(
        repository: Repository,
        ai: Arc<dyn AiProvider>,
        storage: Arc<dyn AssetStore>,
        renderer: RenderClient,
        asset_root: PathBuf,
    ) -> Self {
        Self {
            repository,
            ai,
            storage,
            renderer,
            asset_root,
        }
    }

    pub fn repository(&self) -> &Repository {
        &self.repository
    }

    #[instrument(skip(self, project), fields(workflow_id = %workflow_id, project_id = %project.id, version_id = %version.id))]
    pub async fn generate_script(
        &self,
        workflow_id: Uuid,
        project: &Project,
        version: &ProjectVersion,
    ) -> AppResult<ScriptSpec> {
        let key = format!(
            "{}:{}:script:{}",
            project.id,
            version.version,
            self.ai.text_model()
        );
        if let Some(cached) = self.cached::<ScriptSpec>(&key).await? {
            return Ok(cached);
        }
        let task_id = self
            .start_task(TaskDescriptor {
                workflow_id,
                project_id: project.id,
                version_id: Some(version.id),
                scene_id: None,
                stage: "script",
                key: &key,
                model: self.ai.text_model(),
            })
            .await?;
        let output = self.ai.generate_script(project, task_id).await;
        match output {
            Ok(output) => {
                self.repository
                    .save_script(version.id, &output.value)
                    .await?;
                self.finish_task(task_id, &output).await?;
                Ok(output.value)
            }
            Err(err) => {
                self.repository.fail_task(task_id, &err.to_string()).await?;
                Err(err)
            }
        }
    }

    #[instrument(skip(self, project, script), fields(workflow_id = %workflow_id, project_id = %project.id, version_id = %version.id))]
    pub async fn generate_storyboard(
        &self,
        workflow_id: Uuid,
        project: &Project,
        version: &ProjectVersion,
        script: &ScriptSpec,
    ) -> AppResult<StoryboardSpec> {
        let input = serde_json::to_vec(script)?;
        let key = format!(
            "{}:{}:storyboard:{}:{}",
            project.id,
            version.version,
            self.ai.text_model(),
            short_hash(&input)
        );
        if let Some(cached) = self.cached::<StoryboardSpec>(&key).await? {
            return Ok(cached);
        }
        let task_id = self
            .start_task(TaskDescriptor {
                workflow_id,
                project_id: project.id,
                version_id: Some(version.id),
                scene_id: None,
                stage: "storyboard",
                key: &key,
                model: self.ai.text_model(),
            })
            .await?;
        let output = self.ai.generate_storyboard(project, script, task_id).await;
        match output {
            Ok(output) => {
                validate_storyboard(&output.value)?;
                self.repository
                    .save_storyboard(version.id, &output.value)
                    .await?;
                self.repository
                    .replace_scenes(project.id, version.id, &output.value.scenes)
                    .await?;
                self.finish_task(task_id, &output).await?;
                Ok(output.value)
            }
            Err(err) => {
                self.repository.fail_task(task_id, &err.to_string()).await?;
                Err(err)
            }
        }
    }

    #[instrument(skip(self, project, style), fields(workflow_id = %workflow_id, project_id = %project.id, scene_id = %scene.id, sequence = scene.sequence))]
    pub async fn generate_scene_assets(
        &self,
        workflow_id: Uuid,
        project: &Project,
        version: &ProjectVersion,
        scene: &SceneRecord,
        style: &crate::domain::StyleBible,
    ) -> AppResult<GeneratedScene> {
        let input = serde_json::to_vec(&(
            scene.narration_text.as_str(),
            scene.visual_prompt.as_str(),
            style,
        ))?;
        let key = format!(
            "{}:{}:scene:{}:{}:{}",
            project.id,
            version.version,
            scene.sequence,
            self.ai.image_model(),
            short_hash(&input)
        );
        if let Some(cached) = self.cached::<GeneratedScene>(&key).await? {
            return Ok(cached);
        }
        let task_id = self
            .start_task(TaskDescriptor {
                workflow_id,
                project_id: project.id,
                version_id: Some(version.id),
                scene_id: Some(scene.id),
                stage: "scene_assets",
                key: &key,
                model: self.ai.image_model(),
            })
            .await?;
        let result = self
            .create_scene_assets(project, scene, style, task_id)
            .await;
        match result {
            Ok(generated) => {
                self.repository
                    .complete_task(task_id, &generated, None)
                    .await?;
                Ok(generated)
            }
            Err(err) => {
                self.repository.fail_task(task_id, &err.to_string()).await?;
                Err(err)
            }
        }
    }

    async fn create_scene_assets(
        &self,
        project: &Project,
        scene: &SceneRecord,
        style: &crate::domain::StyleBible,
        task_id: Uuid,
    ) -> AppResult<GeneratedScene> {
        let portrait = project.aspect_ratio == "9:16";
        let prompt = format!(
            "{}\n统一艺术方向：{}\n色板：{}\n画面规则：{}\n禁止内容：{}\n不要生成任何文字、水印或标志。",
            scene.visual_prompt,
            style.art_direction,
            style.color_palette.join(", "),
            style.image_rules.join("；"),
            style.negative_prompt
        );
        let image = self.ai.generate_image(&prompt, portrait, task_id).await?;
        let speech = self
            .ai
            .synthesize_speech(&scene.narration_text, &project.voice, task_id)
            .await?;

        let base_key = format!("projects/{}/scenes/{}", project.id, scene.id);
        let image_object = self
            .storage
            .put(
                &format!("{base_key}/illustration.png"),
                "image/png",
                &image.value,
            )
            .await?;
        let audio_key = format!("{base_key}/narration.wav");
        let audio_object = self
            .storage
            .put(&audio_key, "audio/wav", &speech.value)
            .await?;
        let duration_ms = probe_duration_ms(self.asset_root.join(&audio_key)).await?;

        let aligned = self
            .ai
            .align_speech(speech.value, &scene.narration_text, task_id)
            .await?;
        let caption_bytes = serde_json::to_vec_pretty(&aligned.value)?;
        let caption_object = self
            .storage
            .put(
                &format!("{base_key}/captions.json"),
                "application/json",
                &caption_bytes,
            )
            .await?;

        let image_asset = self
            .insert_asset(
                project.id,
                scene.id,
                "image",
                self.ai.image_model(),
                &image_object,
                json!({"usage": image.usage}),
            )
            .await?;
        let audio_asset = self
            .insert_asset(
                project.id,
                scene.id,
                "audio",
                self.ai.tts_model(),
                &audio_object,
                json!({"usage": speech.usage, "duration_ms": duration_ms}),
            )
            .await?;
        let caption_asset = self
            .insert_asset(
                project.id,
                scene.id,
                "captions",
                "whisper-1",
                &caption_object,
                json!({"usage": aligned.usage}),
            )
            .await?;
        self.repository
            .mark_scene_ready(
                scene.id,
                duration_ms,
                image_asset.id,
                audio_asset.id,
                caption_asset.id,
            )
            .await?;

        Ok(GeneratedScene {
            scene_id: scene.id,
            sequence: scene.sequence,
            narration: scene.narration_text.clone(),
            visual_type: parse_visual_type(&scene.visual_type),
            image_url: image_object.public_url,
            audio_url: audio_object.public_url,
            on_screen_text: scene.on_screen_text.clone(),
            transition: parse_transition(scene.metadata.get("transition").and_then(Value::as_str)),
            duration_ms,
            captions: aligned.value,
        })
    }

    async fn insert_asset(
        &self,
        project_id: Uuid,
        scene_id: Uuid,
        kind: &str,
        model: &str,
        object: &crate::storage::StoredObject,
        metadata: Value,
    ) -> AppResult<AssetRecord> {
        self.repository
            .insert_asset(&NewAsset {
                id: Uuid::new_v4(),
                project_id,
                scene_id: Some(scene_id),
                kind: kind.into(),
                provider: self.ai.name().into(),
                provider_asset_id: Some(model.into()),
                storage_key: object.key.clone(),
                public_url: object.public_url.clone(),
                content_type: match kind {
                    "image" => "image/png",
                    "audio" => "audio/wav",
                    _ => "application/json",
                }
                .into(),
                byte_size: object.byte_size,
                checksum: object.checksum.clone(),
                metadata,
            })
            .await
    }

    pub fn build_render_spec(
        &self,
        project: &Project,
        version: &ProjectVersion,
        generated: Vec<GeneratedScene>,
    ) -> RenderSpec {
        let (width, height) = match project.aspect_ratio.as_str() {
            "9:16" => (1080, 1920),
            "1:1" => (1080, 1080),
            _ => (1920, 1080),
        };
        let fps = 30;
        let mut cursor = 0_i64;
        let scenes = generated
            .into_iter()
            .map(|scene| {
                let duration_in_frames =
                    ((scene.duration_ms + 250) * fps as i64 / 1000).max(fps as i64);
                let output = RenderScene {
                    id: scene.scene_id,
                    sequence: scene.sequence,
                    start_frame: cursor,
                    duration_in_frames,
                    narration: scene.narration,
                    visual_type: scene.visual_type,
                    image_url: scene.image_url,
                    audio_url: scene.audio_url,
                    on_screen_text: scene.on_screen_text,
                    transition: scene.transition,
                    captions: scene.captions,
                };
                cursor += duration_in_frames;
                output
            })
            .collect();
        RenderSpec {
            project_id: project.id,
            version: version.version,
            width,
            height,
            fps,
            background_color: "#0B1020".into(),
            scenes,
        }
    }

    #[instrument(skip(self, spec), fields(workflow_id = %workflow_id, project_id = %project.id, version_id = %version.id))]
    pub async fn render_video(
        &self,
        workflow_id: Uuid,
        project: &Project,
        version: &ProjectVersion,
        spec: RenderSpec,
    ) -> AppResult<String> {
        let bytes = serde_json::to_vec(&spec)?;
        let hash = blake3::hash(&bytes).to_hex().to_string();
        let key = format!("{}:{}:render:{}", project.id, version.version, &hash[..16]);
        if let Some(cached) = self.cached::<RenderCache>(&key).await? {
            return Ok(cached.public_url);
        }
        let render_id = Uuid::new_v4();
        self.repository
            .insert_render(&NewRender {
                id: render_id,
                project_id: project.id,
                project_version_id: version.id,
                workflow_id,
                status: "rendering".into(),
                render_spec_hash: hash,
                width: spec.width,
                height: spec.height,
                fps: spec.fps,
            })
            .await?;
        let task_id = self
            .start_task(TaskDescriptor {
                workflow_id,
                project_id: project.id,
                version_id: Some(version.id),
                scene_id: None,
                stage: "render",
                key: &key,
                model: "remotion",
            })
            .await?;
        let request = RenderJobRequest {
            render_id,
            output_key: format!("projects/{}/renders/{render_id}.mp4", project.id),
            spec,
        };
        let output = self.renderer.render(&request).await;
        match output {
            Ok(result) => {
                self.repository
                    .complete_render(
                        render_id,
                        &result.storage_key,
                        &result.public_url,
                        result.duration_ms,
                    )
                    .await?;
                let cached = RenderCache {
                    public_url: result.public_url,
                };
                self.repository
                    .complete_task(task_id, &cached, None)
                    .await?;
                Ok(cached.public_url)
            }
            Err(err) => {
                self.repository.fail_task(task_id, &err.to_string()).await?;
                Err(err)
            }
        }
    }

    async fn cached<T: DeserializeOwned>(&self, key: &str) -> AppResult<Option<T>> {
        self.repository
            .find_succeeded_task(key)
            .await?
            .map(serde_json::from_value)
            .transpose()
            .map_err(Into::into)
    }

    async fn start_task(&self, task: TaskDescriptor<'_>) -> AppResult<Uuid> {
        let TaskDescriptor {
            workflow_id,
            project_id,
            version_id,
            scene_id,
            stage,
            key,
            model,
        } = task;
        info!(workflow_id = %workflow_id, project_id = %project_id, ?scene_id, stage, idempotency_key = key, model, "生成阶段开始");
        self.repository
            .start_task(&NewGenerationTask {
                id: Uuid::new_v4(),
                workflow_id,
                project_id,
                project_version_id: version_id,
                scene_id,
                stage: stage.into(),
                status: "running".into(),
                attempt: 1,
                idempotency_key: key.into(),
                provider: Some(self.ai.name().into()),
                model: Some(model.into()),
                input_hash: short_hash(key.as_bytes()),
            })
            .await
    }

    async fn finish_task<T: Serialize>(
        &self,
        task_id: Uuid,
        output: &ProviderOutput<T>,
    ) -> AppResult<()> {
        self.repository
            .complete_task(
                task_id,
                &output.value,
                Some(serde_json::to_value(&output.usage)?),
            )
            .await
    }
}

struct TaskDescriptor<'a> {
    workflow_id: Uuid,
    project_id: Uuid,
    version_id: Option<Uuid>,
    scene_id: Option<Uuid>,
    stage: &'a str,
    key: &'a str,
    model: &'a str,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GeneratedScene {
    pub scene_id: Uuid,
    pub sequence: i32,
    pub narration: String,
    pub visual_type: VisualType,
    pub image_url: String,
    pub audio_url: String,
    pub on_screen_text: Option<String>,
    pub transition: TransitionKind,
    pub duration_ms: i64,
    pub captions: Vec<CaptionCue>,
}

#[derive(Serialize, Deserialize)]
struct RenderCache {
    public_url: String,
}

fn validate_storyboard(storyboard: &StoryboardSpec) -> AppResult<()> {
    if storyboard.scenes.is_empty() {
        return Err(AppError::Validation("分镜不能为空".into()));
    }
    if storyboard.scenes.len() > 300 {
        return Err(AppError::Validation("分镜不能超过 300 个场景".into()));
    }
    for (index, scene) in storyboard.scenes.iter().enumerate() {
        if scene.sequence != index as i32 + 1 {
            return Err(AppError::Validation(format!(
                "场景序号必须从 1 连续递增，错误位置 {}",
                index + 1
            )));
        }
        if scene.narration.trim().is_empty() {
            return Err(AppError::Validation(format!(
                "场景 {} 缺少口播",
                scene.sequence
            )));
        }
    }
    Ok(())
}

fn short_hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex()[..16].to_owned()
}

fn parse_visual_type(value: &str) -> VisualType {
    match value {
        "infographic" => VisualType::Infographic,
        "quote" => VisualType::Quote,
        "title" => VisualType::Title,
        "list" => VisualType::List,
        _ => VisualType::Illustration,
    }
}

fn parse_transition(value: Option<&str>) -> TransitionKind {
    match value {
        Some("slide") => TransitionKind::Slide,
        Some("wipe") => TransitionKind::Wipe,
        Some("none") => TransitionKind::None,
        _ => TransitionKind::Fade,
    }
}
