use std::{path::PathBuf, sync::Arc};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use tokio::task::JoinSet;
use tracing::{info, instrument};
use uuid::Uuid;

use crate::{
    AppError, AppResult,
    db::{AssetRecord, NewAsset, NewGenerationTask, NewRender, Repository, SceneRecord},
    domain::{
        CaptionCue, Project, ProjectVersion, RenderJobRequest, RenderScene, RenderSpec, ScriptSpec,
        StoryboardSpec, TransitionKind,
    },
    media::probe_duration_ms,
    providers::{ImageGenerationRequest, ModelGatewayClient, ProviderOutput, ProviderUsage},
    render::RenderClient,
    storage::{AssetStore, StoredObject},
};

const SCRIPT_PROMPT_VERSION: &str = "script-v1";
const STORYBOARD_PROMPT_VERSION: &str = "storyboard-v1";
const IMAGE_PROMPT_VERSION: &str = "image-v1";
const SPEECH_PROMPT_VERSION: &str = "speech-v1";
const TRANSCRIPTION_PROMPT_VERSION: &str = "transcription-v1";
const RENDER_PIPELINE_VERSION: &str = "render-v1";
const SCENE_CONCURRENCY: usize = 3;

#[derive(Clone)]
pub struct PipelineService {
    repository: Repository,
    gateway: ModelGatewayClient,
    storage: Arc<dyn AssetStore>,
    renderer: RenderClient,
    asset_root: PathBuf,
}

impl PipelineService {
    pub fn new(
        repository: Repository,
        gateway: ModelGatewayClient,
        storage: Arc<dyn AssetStore>,
        renderer: RenderClient,
        asset_root: PathBuf,
    ) -> Self {
        Self {
            repository,
            gateway,
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
        let key = stable_task_key(
            "script",
            &format!(
                "{}:{}:script:{}:{}:{}",
                project.id,
                version.version,
                project.text_provider,
                project.text_model,
                SCRIPT_PROMPT_VERSION,
            ),
        );
        if let Some(cached) = self.cached::<ScriptSpec>(&key).await? {
            validate_script(&cached)?;
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
                provider: &project.text_provider,
                model: &project.text_model,
            })
            .await?;
        let result = async {
            let output = self
                .gateway
                .generate_script(
                    project,
                    &project.text_provider,
                    &project.text_model,
                    task_id,
                )
                .await?;
            validate_script(&output.value)?;
            self.repository
                .save_script(version.id, &output.value)
                .await?;
            AppResult::Ok(output)
        }
        .await;
        match result {
            Ok(output) => {
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
        let key = stable_task_key(
            "storyboard",
            &format!(
                "{}:{}:storyboard:{}:{}:{}:{}",
                project.id,
                version.version,
                project.text_provider,
                project.text_model,
                STORYBOARD_PROMPT_VERSION,
                content_hash(&input)
            ),
        );
        if let Some(cached) = self.cached::<StoryboardSpec>(&key).await? {
            validate_storyboard(&cached, script)?;
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
                provider: &project.text_provider,
                model: &project.text_model,
            })
            .await?;
        let result = async {
            let output = self
                .gateway
                .generate_storyboard(
                    project,
                    script,
                    &project.text_provider,
                    &project.text_model,
                    task_id,
                )
                .await?;
            validate_storyboard(&output.value, script)?;
            self.repository
                .save_storyboard(version.id, &output.value)
                .await?;
            self.repository
                .replace_scenes(project.id, version.id, &output.value.scenes)
                .await?;
            AppResult::Ok(output)
        }
        .await;
        match result {
            Ok(output) => {
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
        self.repository.mark_scene_generating(scene.id).await?;
        let result = self
            .create_scene_assets(workflow_id, project, version, scene, style)
            .await;
        if let Err(error) = &result {
            self.repository.mark_scene_failed(scene.id).await?;
            info!(scene_id = %scene.id, error = %error, "场景素材生成失败");
        }
        result
    }

    /// 有界并发生成场景，既缩短长视频耗时，也避免同时压满图片与语音 Provider。
    /// 已启动的任务全部等待到明确成功或失败后才返回，保证任务状态可用于排障。
    #[instrument(skip(self, project, version, scenes, style), fields(workflow_id = %workflow_id, project_id = %project.id, scene_count = scenes.len()))]
    pub async fn generate_all_scene_assets(
        &self,
        workflow_id: Uuid,
        project: Project,
        version: ProjectVersion,
        scenes: Vec<SceneRecord>,
        style: crate::domain::StyleBible,
    ) -> AppResult<Vec<GeneratedScene>> {
        let mut pending = scenes.into_iter();
        let mut running = JoinSet::new();
        let mut generated = Vec::new();
        let mut first_error = None;

        for _ in 0..SCENE_CONCURRENCY {
            let Some(scene) = pending.next() else { break };
            spawn_scene_task(
                &mut running,
                self.clone(),
                workflow_id,
                project.clone(),
                version.clone(),
                scene,
                style.clone(),
            );
        }

        while let Some(result) = running.join_next().await {
            match result {
                Ok(Ok(scene)) => generated.push(scene),
                Ok(Err(error)) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
                Err(error) => {
                    if first_error.is_none() {
                        first_error =
                            Some(AppError::Internal(format!("场景生成任务异常终止: {error}")));
                    }
                }
            }

            // 发生错误后不再启动新场景，但继续等待已启动任务完成并写入终态。
            if first_error.is_none()
                && let Some(scene) = pending.next()
            {
                spawn_scene_task(
                    &mut running,
                    self.clone(),
                    workflow_id,
                    project.clone(),
                    version.clone(),
                    scene,
                    style.clone(),
                );
            }
        }

        if let Some(error) = first_error {
            return Err(error);
        }
        generated.sort_by_key(|scene| scene.sequence);
        info!(workflow_id = %workflow_id, project_id = %project.id, scene_count = generated.len(), "全部场景素材生成完成");
        Ok(generated)
    }

    async fn create_scene_assets(
        &self,
        workflow_id: Uuid,
        project: &Project,
        version: &ProjectVersion,
        scene: &SceneRecord,
        style: &crate::domain::StyleBible,
    ) -> AppResult<GeneratedScene> {
        // 图片与口播互不依赖，可以并行执行；使用 join 等待两边都收口任务状态，
        // 避免一边失败后取消另一边，使 generation_tasks 永久残留 running。
        let (image, speech) = tokio::join!(
            self.generate_image_asset(workflow_id, project, version, scene, style),
            self.generate_speech_asset(workflow_id, project, version, scene),
        );
        let image = image?;
        let speech = speech?;
        let captions = self
            .generate_caption_asset(workflow_id, project, version, scene, &speech)
            .await?;

        self.repository
            .mark_scene_ready(
                scene.id,
                speech.duration_ms,
                image.asset_id,
                speech.asset.asset_id,
                captions.asset_id,
            )
            .await?;

        Ok(GeneratedScene {
            scene_id: scene.id,
            sequence: scene.sequence,
            image_url: image.object.public_url,
            audio_url: speech.asset.object.public_url,
            on_screen_text: scene.on_screen_text.clone(),
            transition: parse_transition(&scene.transition),
            duration_ms: speech.duration_ms,
            captions: captions.cues,
        })
    }

    async fn generate_image_asset(
        &self,
        workflow_id: Uuid,
        project: &Project,
        version: &ProjectVersion,
        scene: &SceneRecord,
        style: &crate::domain::StyleBible,
    ) -> AppResult<GeneratedAsset> {
        let (width, height) = render_dimensions(&project.aspect_ratio);
        let prompt = format!(
            "{}\n统一艺术方向：{}\n色板：{}\n画面规则：{}\n禁止内容：{}\n不要生成任何文字、水印或标志。",
            scene.visual_prompt,
            style.art_direction,
            style.color_palette.join(", "),
            style.image_rules.join("；"),
            style.negative_prompt
        );
        let key = stable_task_key(
            "image",
            &format!(
                "{}:{}:scene:{}:image:{}:{}:{}:{}",
                project.id,
                version.version,
                scene.id,
                project.image_provider,
                project.image_model,
                IMAGE_PROMPT_VERSION,
                content_hash(prompt.as_bytes()),
            ),
        );
        if let Some(cached) = self.cached::<GeneratedAsset>(&key).await? {
            return Ok(cached);
        }
        let task_id = self
            .start_task(TaskDescriptor {
                workflow_id,
                project_id: project.id,
                version_id: Some(version.id),
                scene_id: Some(scene.id),
                stage: "image",
                key: &key,
                provider: &project.image_provider,
                model: &project.image_model,
            })
            .await?;
        let result: AppResult<(GeneratedAsset, ProviderUsage)> = async {
            let output = self
                .gateway
                .generate_image(ImageGenerationRequest {
                    prompt: &prompt,
                    width,
                    height,
                    quality: "high",
                    provider: &project.image_provider,
                    model: &project.image_model,
                    request_id: task_id,
                })
                .await?;
            let base_key = format!("projects/{}/scenes/{}", project.id, scene.id);
            let object = self
                .storage
                .put(
                    &format!("{base_key}/illustration.png"),
                    "image/png",
                    &output.value,
                )
                .await?;
            let asset = self
                .insert_asset(
                    project.id,
                    scene.id,
                    AssetDescriptor {
                        kind: "image",
                        provider: &project.image_provider,
                        model: &project.image_model,
                        object: &object,
                        metadata: json!({"width": width, "height": height, "quality": "high"}),
                    },
                )
                .await?;
            Ok((
                GeneratedAsset {
                    asset_id: asset.id,
                    object,
                },
                output.usage,
            ))
        }
        .await;
        match result {
            Ok((cached, usage)) => {
                self.complete_cached_task(task_id, &cached, usage).await?;
                Ok(cached)
            }
            Err(error) => self.fail_task(task_id, error).await,
        }
    }

    async fn generate_speech_asset(
        &self,
        workflow_id: Uuid,
        project: &Project,
        version: &ProjectVersion,
        scene: &SceneRecord,
    ) -> AppResult<GeneratedSpeech> {
        let key = stable_task_key(
            "speech",
            &format!(
                "{}:{}:scene:{}:speech:{}:{}:{}:{}:{}",
                project.id,
                version.version,
                scene.id,
                project.speech_provider,
                project.speech_model,
                project.voice,
                SPEECH_PROMPT_VERSION,
                content_hash(scene.narration_text.as_bytes()),
            ),
        );
        if let Some(cached) = self.cached::<GeneratedSpeech>(&key).await? {
            return Ok(cached);
        }
        let task_id = self
            .start_task(TaskDescriptor {
                workflow_id,
                project_id: project.id,
                version_id: Some(version.id),
                scene_id: Some(scene.id),
                stage: "speech",
                key: &key,
                provider: &project.speech_provider,
                model: &project.speech_model,
            })
            .await?;
        let result: AppResult<(GeneratedSpeech, ProviderUsage)> = async {
            let output = self
                .gateway
                .synthesize_speech(
                    &scene.narration_text,
                    &project.voice,
                    &project.speech_provider,
                    &project.speech_model,
                    task_id,
                )
                .await?;
            let base_key = format!("projects/{}/scenes/{}", project.id, scene.id);
            let audio_key = format!("{base_key}/narration.wav");
            let object = self
                .storage
                .put(&audio_key, "audio/wav", &output.value)
                .await?;
            let duration_ms = probe_duration_ms(self.asset_root.join(&audio_key)).await?;
            let asset = self
                .insert_asset(
                    project.id,
                    scene.id,
                    AssetDescriptor {
                        kind: "audio",
                        provider: &project.speech_provider,
                        model: &project.speech_model,
                        object: &object,
                        metadata: json!({"duration_ms": duration_ms}),
                    },
                )
                .await?;
            Ok((
                GeneratedSpeech {
                    asset: GeneratedAsset {
                        asset_id: asset.id,
                        object,
                    },
                    duration_ms,
                },
                output.usage,
            ))
        }
        .await;
        match result {
            Ok((cached, usage)) => {
                self.complete_cached_task(task_id, &cached, usage).await?;
                Ok(cached)
            }
            Err(error) => self.fail_task(task_id, error).await,
        }
    }

    async fn generate_caption_asset(
        &self,
        workflow_id: Uuid,
        project: &Project,
        version: &ProjectVersion,
        scene: &SceneRecord,
        speech: &GeneratedSpeech,
    ) -> AppResult<GeneratedCaptions> {
        let key = stable_task_key(
            "transcription",
            &format!(
                "{}:{}:scene:{}:captions:{}:{}:{}:{}:{}",
                project.id,
                version.version,
                scene.id,
                project.transcription_provider,
                project.transcription_model,
                TRANSCRIPTION_PROMPT_VERSION,
                speech.asset.object.checksum,
                content_hash(scene.narration_text.as_bytes()),
            ),
        );
        if let Some(cached) = self.cached::<GeneratedCaptions>(&key).await? {
            return Ok(cached);
        }
        let task_id = self
            .start_task(TaskDescriptor {
                workflow_id,
                project_id: project.id,
                version_id: Some(version.id),
                scene_id: Some(scene.id),
                stage: "transcription",
                key: &key,
                provider: &project.transcription_provider,
                model: &project.transcription_model,
            })
            .await?;
        let result: AppResult<(GeneratedCaptions, ProviderUsage)> = async {
            let audio = tokio::fs::read(self.asset_root.join(&speech.asset.object.key)).await?;
            let output = self
                .gateway
                .align_speech(
                    audio,
                    &scene.narration_text,
                    &project.transcription_provider,
                    &project.transcription_model,
                    task_id,
                )
                .await?;
            validate_captions(&output.value, speech.duration_ms)?;
            let caption_bytes = serde_json::to_vec_pretty(&output.value)?;
            let base_key = format!("projects/{}/scenes/{}", project.id, scene.id);
            let object = self
                .storage
                .put(
                    &format!("{base_key}/captions.json"),
                    "application/json",
                    &caption_bytes,
                )
                .await?;
            let asset = self
                .insert_asset(
                    project.id,
                    scene.id,
                    AssetDescriptor {
                        kind: "captions",
                        provider: &project.transcription_provider,
                        model: &project.transcription_model,
                        object: &object,
                        metadata: json!({"cue_count": output.value.len()}),
                    },
                )
                .await?;
            Ok((
                GeneratedCaptions {
                    asset_id: asset.id,
                    object,
                    cues: output.value,
                },
                output.usage,
            ))
        }
        .await;
        match result {
            Ok((cached, usage)) => {
                self.complete_cached_task(task_id, &cached, usage).await?;
                Ok(cached)
            }
            Err(error) => self.fail_task(task_id, error).await,
        }
    }

    async fn insert_asset(
        &self,
        project_id: Uuid,
        scene_id: Uuid,
        asset: AssetDescriptor<'_>,
    ) -> AppResult<AssetRecord> {
        let AssetDescriptor {
            kind,
            provider,
            model,
            object,
            metadata,
        } = asset;
        self.repository
            .insert_asset(&NewAsset {
                id: Uuid::new_v4(),
                project_id,
                scene_id: Some(scene_id),
                kind: kind.into(),
                provider: provider.into(),
                model: model.into(),
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
        let (width, height) = render_dimensions(&project.aspect_ratio);
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
        let key = stable_task_key(
            "render",
            &format!(
                "{}:{}:render:{}:{}",
                project.id, version.version, RENDER_PIPELINE_VERSION, hash
            ),
        );
        if let Some(cached) = self.cached::<RenderCache>(&key).await? {
            return Ok(cached.public_url);
        }
        let task_id = self
            .start_task(TaskDescriptor {
                workflow_id,
                project_id: project.id,
                version_id: Some(version.id),
                scene_id: None,
                stage: "render",
                key: &key,
                provider: "remotion",
                model: "remotion",
            })
            .await?;
        // 渲染任务 ID 来自持久化 generation_tasks，同一幂等键重试时保持不变。
        let render_id = self
            .repository
            .start_render(&NewRender {
                id: task_id,
                project_id: project.id,
                project_version_id: version.id,
                workflow_id,
                status: "rendering".into(),
                render_spec_hash: hash.clone(),
                width: spec.width,
                height: spec.height,
                fps: spec.fps,
            })
            .await;
        let render_id = match render_id {
            Ok(render_id) => render_id,
            Err(error) => return self.fail_task(task_id, error).await,
        };
        let request = RenderJobRequest {
            render_id,
            output_key: format!("projects/{}/renders/{hash}.mp4", project.id),
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
                self.repository
                    .fail_render(render_id, &err.to_string())
                    .await?;
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
            provider,
            model,
        } = task;
        info!(workflow_id = %workflow_id, project_id = %project_id, ?scene_id, stage, idempotency_key = key, provider, model, "生成阶段开始");
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
                provider: Some(provider.into()),
                model: Some(model.into()),
            })
            .await
    }

    async fn complete_cached_task<T: Serialize>(
        &self,
        task_id: Uuid,
        value: &T,
        usage: ProviderUsage,
    ) -> AppResult<()> {
        self.repository
            .complete_task(task_id, value, Some(serde_json::to_value(usage)?))
            .await
    }

    /// 无论错误发生在模型调用、响应校验、素材落盘还是元数据写入，都终结对应任务。
    /// 这样排障时不会看到已经失败的工作流残留 `running` 任务。
    async fn fail_task<T>(&self, task_id: Uuid, error: AppError) -> AppResult<T> {
        self.repository
            .fail_task(task_id, &error.to_string())
            .await?;
        Err(error)
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
    provider: &'a str,
    model: &'a str,
}

/// 将素材来源和存储结果组合传递，避免图片、口播、字幕写入时混淆各自的 Provider。
struct AssetDescriptor<'a> {
    kind: &'a str,
    provider: &'a str,
    model: &'a str,
    object: &'a crate::storage::StoredObject,
    metadata: Value,
}

fn spawn_scene_task(
    tasks: &mut JoinSet<AppResult<GeneratedScene>>,
    pipeline: PipelineService,
    workflow_id: Uuid,
    project: Project,
    version: ProjectVersion,
    scene: SceneRecord,
    style: crate::domain::StyleBible,
) {
    tasks.spawn(async move {
        pipeline
            .generate_scene_assets(workflow_id, &project, &version, &scene, &style)
            .await
    });
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GeneratedScene {
    pub scene_id: Uuid,
    pub sequence: i32,
    pub image_url: String,
    pub audio_url: String,
    pub on_screen_text: Option<String>,
    pub transition: TransitionKind,
    pub duration_ms: i64,
    pub captions: Vec<CaptionCue>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct GeneratedAsset {
    asset_id: Uuid,
    object: StoredObject,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct GeneratedSpeech {
    asset: GeneratedAsset,
    duration_ms: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct GeneratedCaptions {
    asset_id: Uuid,
    object: StoredObject,
    cues: Vec<CaptionCue>,
}

#[derive(Serialize, Deserialize)]
struct RenderCache {
    public_url: String,
}

fn validate_script(script: &ScriptSpec) -> AppResult<()> {
    if script.title.trim().is_empty()
        || script.summary.trim().is_empty()
        || script.visual_style.trim().is_empty()
        || script.full_narration.trim().is_empty()
        || script.sections.is_empty()
    {
        return Err(AppError::Validation("文稿关键字段不能为空".into()));
    }
    let section_narration = script
        .sections
        .iter()
        .map(|section| section.narration.as_str())
        .collect::<String>();
    if compact_text(&section_narration) != compact_text(&script.full_narration) {
        return Err(AppError::Validation(
            "文稿 sections 的口播内容与 full_narration 不一致".into(),
        ));
    }
    Ok(())
}

fn validate_storyboard(storyboard: &StoryboardSpec, script: &ScriptSpec) -> AppResult<()> {
    if storyboard.style_bible.art_direction.trim().is_empty()
        || storyboard.style_bible.color_palette.is_empty()
        || storyboard.style_bible.image_rules.is_empty()
        || storyboard.style_bible.negative_prompt.trim().is_empty()
    {
        return Err(AppError::Validation("分镜视觉规范不能为空".into()));
    }
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
        if scene.visual_prompt.trim().is_empty() {
            return Err(AppError::Validation(format!(
                "场景 {} 缺少画面提示词",
                scene.sequence
            )));
        }
    }
    let storyboard_narration = storyboard
        .scenes
        .iter()
        .map(|scene| scene.narration.as_str())
        .collect::<String>();
    if compact_text(&storyboard_narration) != compact_text(&script.full_narration) {
        return Err(AppError::Validation(
            "分镜口播合并后与审核文稿不一致".into(),
        ));
    }
    Ok(())
}

fn compact_text(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

fn validate_captions(cues: &[CaptionCue], duration_ms: i64) -> AppResult<()> {
    let mut previous_end = 0;
    for (index, cue) in cues.iter().enumerate() {
        if cue.text.trim().is_empty()
            || cue.start_ms < 0
            || cue.end_ms <= cue.start_ms
            || cue.start_ms < previous_end
            || cue.end_ms > duration_ms + 1_000
        {
            return Err(AppError::Validation(format!(
                "字幕时间戳无效，错误位置 {}: {}-{}ms",
                index + 1,
                cue.start_ms,
                cue.end_ms
            )));
        }
        previous_end = cue.end_ms;
    }
    Ok(())
}

fn render_dimensions(aspect_ratio: &str) -> (i32, i32) {
    match aspect_ratio {
        "9:16" => (1080, 1920),
        "1:1" => (1080, 1080),
        _ => (1920, 1080),
    }
}

fn content_hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

fn stable_task_key(stage: &str, material: &str) -> String {
    // Provider 和模型名称允许较长，统一哈希后可严格满足数据库 VARCHAR(256) 约束。
    format!("{stage}:{}", blake3::hash(material.as_bytes()).to_hex())
}

fn parse_transition(value: &str) -> TransitionKind {
    match value {
        "slide" => TransitionKind::Slide,
        "wipe" => TransitionKind::Wipe,
        "none" => TransitionKind::None,
        _ => TransitionKind::Fade,
    }
}
