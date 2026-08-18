use std::sync::Arc;

use chrono::{DateTime, Utc};
use diesel::{
    Connection, ExpressionMethods, Insertable, OptionalExtension, PgConnection, QueryDsl,
    Queryable, Selectable, SelectableHelper,
};
use diesel_async::{
    AsyncConnection, AsyncPgConnection, RunQueryDsl,
    pooled_connection::{AsyncDieselConnectionManager, bb8::Pool},
};
use diesel_migrations::{EmbeddedMigrations, MigrationHarness, embed_migrations};
use serde::Serialize;
use serde_json::Value;
use tracing::{debug, info, instrument, warn};
use uuid::Uuid;

use crate::{
    AppError, AppResult,
    domain::{CreateProjectRequest, NewProject, Project, ProjectVersion, SceneDraft},
    schema::{assets, generation_tasks, project_versions, projects, renders, scenes},
};

pub type DbPool = Pool<AsyncPgConnection>;
pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!("../../migrations");

pub async fn create_pool(database_url: &str) -> AppResult<DbPool> {
    let manager = AsyncDieselConnectionManager::<AsyncPgConnection>::new(database_url);
    Pool::builder()
        .max_size(20)
        .build(manager)
        .await
        .map_err(|err| AppError::Pool(err.to_string()))
}

pub async fn run_migrations(database_url: String) -> AppResult<()> {
    tokio::task::spawn_blocking(move || -> AppResult<()> {
        let mut connection = PgConnection::establish(&database_url)
            .map_err(|err| AppError::Pool(format!("连接 PostgreSQL 失败: {err}")))?;
        connection
            .run_pending_migrations(MIGRATIONS)
            .map_err(|err| AppError::Internal(format!("执行数据库迁移失败: {err}")))?;
        Ok(())
    })
    .await
    .map_err(|err| AppError::Internal(format!("迁移任务异常结束: {err}")))??;
    info!("数据库迁移检查完成");
    Ok(())
}

#[derive(Clone)]
pub struct Repository {
    pool: Arc<DbPool>,
}

impl Repository {
    pub fn new(pool: DbPool) -> Self {
        Self {
            pool: Arc::new(pool),
        }
    }

    async fn connection(
        &self,
    ) -> AppResult<diesel_async::pooled_connection::bb8::PooledConnection<'_, AsyncPgConnection>>
    {
        self.pool
            .get()
            .await
            .map_err(|err| AppError::Pool(err.to_string()))
    }

    #[instrument(skip(self, request), fields(project_id = tracing::field::Empty))]
    pub async fn create_project(&self, request: &CreateProjectRequest) -> AppResult<Project> {
        request.validate()?;
        let id = Uuid::new_v4();
        tracing::Span::current().record("project_id", tracing::field::display(id));
        let new_project = NewProject {
            id,
            title: request.title.trim(),
            source_text: request.source_text.trim(),
            language: &request.language,
            aspect_ratio: request.aspect_ratio.database_value(),
            target_duration_seconds: request.target_duration_seconds,
            text_provider: &request.text_provider,
            text_model: &request.text_model,
            image_provider: &request.image_provider,
            image_model: &request.image_model,
            voice: &request.voice,
            speech_provider: &request.speech_provider,
            speech_model: &request.speech_model,
            transcription_provider: &request.transcription_provider,
            transcription_model: &request.transcription_model,
            require_script_review: request.require_script_review,
        };
        let mut conn = self.connection().await?;
        let project = diesel::insert_into(projects::table)
            .values(new_project)
            .returning(Project::as_returning())
            .get_result(&mut conn)
            .await?;
        info!(
            project_id = %id,
            text_provider = request.text_provider,
            text_model = request.text_model,
            image_provider = request.image_provider,
            image_model = request.image_model,
            speech_provider = request.speech_provider,
            speech_model = request.speech_model,
            transcription_provider = request.transcription_provider,
            transcription_model = request.transcription_model,
            "项目已创建"
        );
        Ok(project)
    }

    pub async fn get_project(&self, project_id: Uuid) -> AppResult<Project> {
        let mut conn = self.connection().await?;
        projects::table
            .find(project_id)
            .select(Project::as_select())
            .first(&mut conn)
            .await
            .optional()?
            .ok_or_else(|| AppError::NotFound(format!("项目 {project_id}")))
    }

    pub async fn update_project_status(
        &self,
        project_id: Uuid,
        workflow_id: Uuid,
        status: &str,
        error_message: Option<&str>,
    ) -> AppResult<()> {
        let mut conn = self.connection().await?;
        // 每次状态变更都绑定活动工作流，防止已经终止的旧工作流覆盖新版本的状态。
        let affected = diesel::update(
            projects::table
                .filter(projects::id.eq(project_id))
                .filter(projects::active_workflow_id.eq(Some(workflow_id))),
        )
        .set((
            projects::status.eq(status),
            projects::error_message.eq(error_message),
            projects::updated_at.eq(Utc::now()),
        ))
        .execute(&mut conn)
        .await?;
        if affected == 0 {
            return Err(AppError::Conflict(format!(
                "工作流 {workflow_id} 已不是项目 {project_id} 的活动工作流"
            )));
        }
        info!(project_id = %project_id, workflow_id = %workflow_id, status, "项目状态已更新");
        Ok(())
    }

    pub async fn queue_project(&self, project_id: Uuid, workflow_id: Uuid) -> AppResult<()> {
        let mut conn = self.connection().await?;
        // 状态判断和排队必须在同一条 UPDATE 中完成，避免两个并发请求都成功启动工作流。
        let affected = diesel::update(
            projects::table
                .filter(projects::id.eq(project_id))
                .filter(projects::status.eq_any(["draft", "failed", "completed"])),
        )
        .set((
            projects::status.eq("queued"),
            projects::active_workflow_id.eq(Some(workflow_id)),
            projects::error_message.eq::<Option<String>>(None),
            projects::updated_at.eq(Utc::now()),
        ))
        .execute(&mut conn)
        .await?;
        if affected == 0 {
            let current = self.get_project(project_id).await?;
            return Err(AppError::Conflict(format!(
                "项目 {} 当前状态为 {}，不能重复启动",
                project_id, current.status
            )));
        }
        info!(project_id = %project_id, workflow_id = %workflow_id, "项目已加入生成队列");
        Ok(())
    }

    /// 写入终态并清除活动工作流。终态项目允许后续显式发起新版本。
    pub async fn finalize_project(
        &self,
        project_id: Uuid,
        workflow_id: Uuid,
        status: &str,
        error_message: Option<&str>,
    ) -> AppResult<()> {
        if !matches!(status, "draft" | "failed" | "completed") {
            return Err(AppError::Internal(format!("{status} 不是项目终态")));
        }
        let mut conn = self.connection().await?;
        let affected = diesel::update(
            projects::table
                .filter(projects::id.eq(project_id))
                .filter(projects::active_workflow_id.eq(Some(workflow_id))),
        )
        .set((
            projects::status.eq(status),
            projects::error_message.eq(error_message),
            projects::active_workflow_id.eq::<Option<Uuid>>(None),
            projects::updated_at.eq(Utc::now()),
        ))
        .execute(&mut conn)
        .await?;
        if affected == 0 {
            return Err(AppError::Conflict(format!(
                "工作流 {workflow_id} 已不是项目 {project_id} 的活动工作流"
            )));
        }
        info!(project_id = %project_id, workflow_id = %workflow_id, status, "项目终态已更新，活动工作流已清除");
        Ok(())
    }

    pub async fn create_project_version(
        &self,
        project_id: Uuid,
        id: Uuid,
        version: i32,
    ) -> AppResult<ProjectVersion> {
        let mut conn = self.connection().await?;
        let result = conn
            .transaction::<ProjectVersion, diesel::result::Error, _>(async move |conn| {
                let result = diesel::insert_into(project_versions::table)
                    .values((
                        project_versions::id.eq(id),
                        project_versions::project_id.eq(project_id),
                        project_versions::version.eq(version),
                    ))
                    .on_conflict((project_versions::project_id, project_versions::version))
                    .do_update()
                    .set(project_versions::updated_at.eq(project_versions::updated_at))
                    .returning(ProjectVersion::as_returning())
                    .get_result(&mut *conn)
                    .await?;
                diesel::update(projects::table.find(project_id))
                    .set((
                        projects::current_version.eq(version),
                        projects::updated_at.eq(Utc::now()),
                    ))
                    .execute(&mut *conn)
                    .await?;
                Ok(result)
            })
            .await?;
        info!(project_id = %project_id, version_id = %result.id, version, "项目版本已创建");
        Ok(result)
    }

    pub async fn latest_version_for_project(
        &self,
        project_id: Uuid,
    ) -> AppResult<Option<ProjectVersion>> {
        let mut conn = self.connection().await?;
        Ok(project_versions::table
            .filter(project_versions::project_id.eq(project_id))
            .order(project_versions::version.desc())
            .select(ProjectVersion::as_select())
            .first(&mut conn)
            .await
            .optional()?)
    }

    pub async fn save_script<T: Serialize>(&self, version_id: Uuid, spec: &T) -> AppResult<()> {
        self.update_version_json(version_id, "script", serde_json::to_value(spec)?)
            .await
    }

    pub async fn save_storyboard<T: Serialize>(&self, version_id: Uuid, spec: &T) -> AppResult<()> {
        self.update_version_json(version_id, "storyboard", serde_json::to_value(spec)?)
            .await
    }

    pub async fn save_render_spec<T: Serialize>(
        &self,
        version_id: Uuid,
        spec: &T,
    ) -> AppResult<()> {
        self.update_version_json(version_id, "render", serde_json::to_value(spec)?)
            .await
    }

    async fn update_version_json(
        &self,
        version_id: Uuid,
        column: &str,
        value: Value,
    ) -> AppResult<()> {
        let mut conn = self.connection().await?;
        let affected = match column {
            "script" => {
                diesel::update(project_versions::table.find(version_id))
                    .set((
                        project_versions::script_spec.eq(value),
                        project_versions::updated_at.eq(Utc::now()),
                    ))
                    .execute(&mut conn)
                    .await?
            }
            "storyboard" => {
                diesel::update(project_versions::table.find(version_id))
                    .set((
                        project_versions::storyboard_spec.eq(value),
                        project_versions::updated_at.eq(Utc::now()),
                    ))
                    .execute(&mut conn)
                    .await?
            }
            "render" => {
                diesel::update(project_versions::table.find(version_id))
                    .set((
                        project_versions::render_spec.eq(value),
                        project_versions::updated_at.eq(Utc::now()),
                    ))
                    .execute(&mut conn)
                    .await?
            }
            _ => return Err(AppError::Internal(format!("未知版本字段 {column}"))),
        };
        if affected == 0 {
            return Err(AppError::NotFound(format!("项目版本 {version_id}")));
        }
        debug!(version_id = %version_id, column, "项目版本内容已保存");
        Ok(())
    }

    pub async fn replace_scenes(
        &self,
        project_id: Uuid,
        version_id: Uuid,
        drafts: &[SceneDraft],
    ) -> AppResult<Vec<SceneRecord>> {
        let mut conn = self.connection().await?;
        let rows: Vec<NewScene> = drafts
            .iter()
            .map(|draft| NewScene {
                id: Uuid::new_v4(),
                project_id,
                project_version_id: version_id,
                sequence: draft.sequence,
                narration_text: draft.narration.clone(),
                visual_prompt: draft.visual_prompt.clone(),
                on_screen_text: draft.on_screen_text.clone(),
                transition: draft.transition.database_value().into(),
            })
            .collect();
        // 删除旧场景和写入新场景必须原子完成，避免 API 在替换期间观察到空分镜。
        let inserted = conn
            .transaction::<Vec<SceneRecord>, diesel::result::Error, _>(async move |conn| {
                diesel::delete(scenes::table.filter(scenes::project_version_id.eq(version_id)))
                    .execute(&mut *conn)
                    .await?;
                diesel::insert_into(scenes::table)
                    .values(&rows)
                    .returning(SceneRecord::as_returning())
                    .get_results(&mut *conn)
                    .await
            })
            .await?;
        info!(project_id = %project_id, version_id = %version_id, count = inserted.len(), "场景记录已写入");
        Ok(inserted)
    }

    pub async fn list_scenes(&self, version_id: Uuid) -> AppResult<Vec<SceneRecord>> {
        let mut conn = self.connection().await?;
        Ok(scenes::table
            .filter(scenes::project_version_id.eq(version_id))
            .order(scenes::sequence.asc())
            .select(SceneRecord::as_select())
            .load(&mut conn)
            .await?)
    }

    pub async fn latest_scenes_for_project(&self, project_id: Uuid) -> AppResult<Vec<SceneRecord>> {
        let project = self.get_project(project_id).await?;
        let mut conn = self.connection().await?;
        let version_id = project_versions::table
            .filter(project_versions::project_id.eq(project_id))
            .filter(project_versions::version.eq(project.current_version))
            .select(project_versions::id)
            .first::<Uuid>(&mut conn)
            .await
            .optional()?;
        match version_id {
            Some(id) => self.list_scenes(id).await,
            None => Ok(Vec::new()),
        }
    }

    pub async fn find_succeeded_task(&self, key: &str) -> AppResult<Option<Value>> {
        let mut conn = self.connection().await?;
        Ok(generation_tasks::table
            .filter(generation_tasks::idempotency_key.eq(key))
            .filter(generation_tasks::status.eq("succeeded"))
            .select(generation_tasks::output)
            .first::<Option<Value>>(&mut conn)
            .await
            .optional()?
            .flatten())
    }

    pub async fn start_task(&self, task: &NewGenerationTask) -> AppResult<Uuid> {
        let mut conn = self.connection().await?;
        diesel::insert_into(generation_tasks::table)
            .values(task)
            .on_conflict(generation_tasks::idempotency_key)
            .do_update()
            .set((
                generation_tasks::status.eq("running"),
                generation_tasks::attempt.eq(generation_tasks::attempt + 1),
                generation_tasks::started_at.eq(Some(Utc::now())),
                generation_tasks::completed_at.eq::<Option<DateTime<Utc>>>(None),
                generation_tasks::error_message.eq::<Option<String>>(None),
                generation_tasks::updated_at.eq(Utc::now()),
            ))
            .returning(generation_tasks::id)
            .get_result(&mut conn)
            .await
            .map_err(Into::into)
    }

    pub async fn complete_task<T: Serialize>(
        &self,
        task_id: Uuid,
        output: &T,
        usage: Option<Value>,
    ) -> AppResult<()> {
        let mut conn = self.connection().await?;
        let affected = diesel::update(generation_tasks::table.find(task_id))
            .set((
                generation_tasks::status.eq("succeeded"),
                generation_tasks::output.eq(serde_json::to_value(output)?),
                generation_tasks::usage.eq(usage),
                generation_tasks::completed_at.eq(Some(Utc::now())),
                generation_tasks::updated_at.eq(Utc::now()),
            ))
            .execute(&mut conn)
            .await?;
        if affected == 0 {
            return Err(AppError::NotFound(format!("生成任务 {task_id}")));
        }
        info!(task_id = %task_id, "生成任务已成功完成");
        Ok(())
    }

    pub async fn fail_task(&self, task_id: Uuid, message: &str) -> AppResult<()> {
        let mut conn = self.connection().await?;
        let affected = diesel::update(generation_tasks::table.find(task_id))
            .set((
                generation_tasks::status.eq("failed"),
                generation_tasks::error_message.eq(Some(message)),
                generation_tasks::completed_at.eq(Some(Utc::now())),
                generation_tasks::updated_at.eq(Utc::now()),
            ))
            .execute(&mut conn)
            .await?;
        if affected == 0 {
            return Err(AppError::NotFound(format!("生成任务 {task_id}")));
        }
        warn!(task_id = %task_id, error = message, "生成任务失败");
        Ok(())
    }

    pub async fn insert_asset(&self, asset: &NewAsset) -> AppResult<AssetRecord> {
        let mut conn = self.connection().await?;
        // 同一存储键重新生成时文件内容已经被覆盖，数据库必须同步新的 Provider、模型和校验和。
        // 保留原记录 ID，避免 scenes 中已保存的素材 ID 失效。
        diesel::insert_into(assets::table)
            .values(asset)
            .on_conflict(assets::storage_key)
            .do_update()
            .set((
                assets::project_id.eq(diesel::upsert::excluded(assets::project_id)),
                assets::scene_id.eq(diesel::upsert::excluded(assets::scene_id)),
                assets::kind.eq(diesel::upsert::excluded(assets::kind)),
                assets::provider.eq(diesel::upsert::excluded(assets::provider)),
                assets::model.eq(diesel::upsert::excluded(assets::model)),
                assets::public_url.eq(diesel::upsert::excluded(assets::public_url)),
                assets::content_type.eq(diesel::upsert::excluded(assets::content_type)),
                assets::byte_size.eq(diesel::upsert::excluded(assets::byte_size)),
                assets::checksum.eq(diesel::upsert::excluded(assets::checksum)),
                assets::metadata.eq(diesel::upsert::excluded(assets::metadata)),
            ))
            .returning(AssetRecord::as_returning())
            .get_result(&mut conn)
            .await
            .map_err(Into::into)
    }

    pub async fn mark_scene_generating(&self, scene_id: Uuid) -> AppResult<()> {
        self.update_scene_status(scene_id, "generating").await
    }

    pub async fn mark_scene_failed(&self, scene_id: Uuid) -> AppResult<()> {
        self.update_scene_status(scene_id, "failed").await
    }

    async fn update_scene_status(&self, scene_id: Uuid, status: &str) -> AppResult<()> {
        let mut conn = self.connection().await?;
        let affected = diesel::update(scenes::table.find(scene_id))
            .set((scenes::status.eq(status), scenes::updated_at.eq(Utc::now())))
            .execute(&mut conn)
            .await?;
        if affected == 0 {
            return Err(AppError::NotFound(format!("场景 {scene_id}")));
        }
        info!(scene_id = %scene_id, status, "场景状态已更新");
        Ok(())
    }

    pub async fn mark_scene_ready(
        &self,
        scene_id: Uuid,
        duration: i64,
        image_id: Uuid,
        audio_id: Uuid,
        caption_id: Uuid,
    ) -> AppResult<()> {
        let mut conn = self.connection().await?;
        diesel::update(scenes::table.find(scene_id))
            .set((
                scenes::status.eq("ready"),
                scenes::duration_ms.eq(Some(duration)),
                scenes::image_asset_id.eq(Some(image_id)),
                scenes::audio_asset_id.eq(Some(audio_id)),
                scenes::caption_asset_id.eq(Some(caption_id)),
                scenes::updated_at.eq(Utc::now()),
            ))
            .execute(&mut conn)
            .await?;
        Ok(())
    }

    /// 创建或恢复同一 RenderSpec 的渲染记录，并始终返回数据库中的真实 ID。
    pub async fn start_render(&self, render: &NewRender) -> AppResult<Uuid> {
        let mut conn = self.connection().await?;
        diesel::insert_into(renders::table)
            .values(render)
            .on_conflict((renders::project_version_id, renders::render_spec_hash))
            .do_update()
            .set((
                renders::workflow_id.eq(diesel::upsert::excluded(renders::workflow_id)),
                renders::status.eq("rendering"),
                renders::storage_key.eq::<Option<String>>(None),
                renders::public_url.eq::<Option<String>>(None),
                renders::duration_ms.eq::<Option<i64>>(None),
                renders::error_message.eq::<Option<String>>(None),
                renders::updated_at.eq(Utc::now()),
            ))
            .returning(renders::id)
            .get_result(&mut conn)
            .await
            .map_err(Into::into)
    }

    pub async fn complete_render(
        &self,
        render_id: Uuid,
        storage_key: &str,
        public_url: &str,
        duration_ms: i64,
    ) -> AppResult<()> {
        let mut conn = self.connection().await?;
        let affected = diesel::update(renders::table.find(render_id))
            .set((
                renders::status.eq("completed"),
                renders::storage_key.eq(Some(storage_key)),
                renders::public_url.eq(Some(public_url)),
                renders::duration_ms.eq(Some(duration_ms)),
                renders::updated_at.eq(Utc::now()),
            ))
            .execute(&mut conn)
            .await?;
        if affected == 0 {
            return Err(AppError::NotFound(format!("渲染记录 {render_id}")));
        }
        Ok(())
    }

    pub async fn fail_render(&self, render_id: Uuid, message: &str) -> AppResult<()> {
        let mut conn = self.connection().await?;
        let affected = diesel::update(renders::table.find(render_id))
            .set((
                renders::status.eq("failed"),
                renders::error_message.eq(Some(message)),
                renders::updated_at.eq(Utc::now()),
            ))
            .execute(&mut conn)
            .await?;
        if affected == 0 {
            return Err(AppError::NotFound(format!("渲染记录 {render_id}")));
        }
        Ok(())
    }

    pub async fn latest_render_for_project(
        &self,
        project_id: Uuid,
    ) -> AppResult<Option<RenderRecord>> {
        let mut conn = self.connection().await?;
        Ok(renders::table
            .filter(renders::project_id.eq(project_id))
            .order(renders::created_at.desc())
            .select(RenderRecord::as_select())
            .first(&mut conn)
            .await
            .optional()?)
    }
}

#[derive(Clone, Debug, Queryable, Selectable, Serialize, serde::Deserialize)]
#[diesel(table_name = scenes)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct SceneRecord {
    pub id: Uuid,
    pub project_id: Uuid,
    pub project_version_id: Uuid,
    pub sequence: i32,
    pub narration_text: String,
    pub visual_prompt: String,
    pub on_screen_text: Option<String>,
    pub transition: String,
    pub status: String,
    pub duration_ms: Option<i64>,
    pub image_asset_id: Option<Uuid>,
    pub audio_asset_id: Option<Uuid>,
    pub caption_asset_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Insertable)]
#[diesel(table_name = scenes)]
struct NewScene {
    id: Uuid,
    project_id: Uuid,
    project_version_id: Uuid,
    sequence: i32,
    narration_text: String,
    visual_prompt: String,
    on_screen_text: Option<String>,
    transition: String,
}

#[derive(Clone, Debug, Insertable)]
#[diesel(table_name = generation_tasks)]
pub struct NewGenerationTask {
    pub id: Uuid,
    pub workflow_id: Uuid,
    pub project_id: Uuid,
    pub project_version_id: Option<Uuid>,
    pub scene_id: Option<Uuid>,
    pub stage: String,
    pub status: String,
    pub attempt: i32,
    pub idempotency_key: String,
    pub provider: Option<String>,
    pub model: Option<String>,
}

#[derive(Clone, Debug, Insertable)]
#[diesel(table_name = assets)]
pub struct NewAsset {
    pub id: Uuid,
    pub project_id: Uuid,
    pub scene_id: Option<Uuid>,
    pub kind: String,
    pub provider: String,
    pub model: String,
    pub storage_key: String,
    pub public_url: String,
    pub content_type: String,
    pub byte_size: i64,
    pub checksum: String,
    pub metadata: Value,
}

#[derive(Clone, Debug, Queryable, Selectable, Serialize)]
#[diesel(table_name = assets)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct AssetRecord {
    pub id: Uuid,
    pub project_id: Uuid,
    pub scene_id: Option<Uuid>,
    pub kind: String,
    pub provider: String,
    pub model: String,
    pub storage_key: String,
    pub public_url: String,
    pub content_type: String,
    pub byte_size: i64,
    pub checksum: String,
    pub metadata: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Insertable)]
#[diesel(table_name = renders)]
pub struct NewRender {
    pub id: Uuid,
    pub project_id: Uuid,
    pub project_version_id: Uuid,
    pub workflow_id: Uuid,
    pub status: String,
    pub render_spec_hash: String,
    pub width: i32,
    pub height: i32,
    pub fps: i32,
}

#[derive(Clone, Debug, Queryable, Selectable, Serialize, serde::Deserialize)]
#[diesel(table_name = renders)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct RenderRecord {
    pub id: Uuid,
    pub project_id: Uuid,
    pub project_version_id: Uuid,
    pub workflow_id: Uuid,
    pub status: String,
    pub render_spec_hash: String,
    pub storage_key: Option<String>,
    pub public_url: Option<String>,
    pub duration_ms: Option<i64>,
    pub width: i32,
    pub height: i32,
    pub fps: i32,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
