// 该文件与迁移保持同步。项目禁止外键，因此不声明 joinable! 关系。
diesel::table! {
    projects (id) {
        id -> Uuid,
        title -> Text,
        source_text -> Text,
        language -> Varchar,
        aspect_ratio -> Varchar,
        target_duration_seconds -> Int4,
        voice -> Varchar,
        speech_provider -> Varchar,
        speech_model -> Varchar,
        transcription_provider -> Varchar,
        transcription_model -> Varchar,
        require_script_review -> Bool,
        status -> Varchar,
        active_workflow_id -> Nullable<Uuid>,
        current_version -> Int4,
        error_message -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    project_versions (id) {
        id -> Uuid,
        project_id -> Uuid,
        version -> Int4,
        script_spec -> Nullable<Jsonb>,
        storyboard_spec -> Nullable<Jsonb>,
        render_spec -> Nullable<Jsonb>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    scenes (id) {
        id -> Uuid,
        project_id -> Uuid,
        project_version_id -> Uuid,
        sequence -> Int4,
        narration_text -> Text,
        visual_type -> Varchar,
        visual_prompt -> Text,
        on_screen_text -> Nullable<Text>,
        status -> Varchar,
        duration_ms -> Nullable<Int8>,
        image_asset_id -> Nullable<Uuid>,
        audio_asset_id -> Nullable<Uuid>,
        caption_asset_id -> Nullable<Uuid>,
        metadata -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    assets (id) {
        id -> Uuid,
        project_id -> Uuid,
        scene_id -> Nullable<Uuid>,
        kind -> Varchar,
        provider -> Varchar,
        provider_asset_id -> Nullable<Text>,
        storage_key -> Text,
        public_url -> Text,
        content_type -> Varchar,
        byte_size -> Int8,
        checksum -> Varchar,
        metadata -> Jsonb,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    generation_tasks (id) {
        id -> Uuid,
        workflow_id -> Uuid,
        project_id -> Uuid,
        project_version_id -> Nullable<Uuid>,
        scene_id -> Nullable<Uuid>,
        stage -> Varchar,
        status -> Varchar,
        attempt -> Int4,
        idempotency_key -> Varchar,
        provider -> Nullable<Varchar>,
        model -> Nullable<Varchar>,
        input_hash -> Varchar,
        output -> Nullable<Jsonb>,
        usage -> Nullable<Jsonb>,
        error_message -> Nullable<Text>,
        started_at -> Nullable<Timestamptz>,
        completed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    renders (id) {
        id -> Uuid,
        project_id -> Uuid,
        project_version_id -> Uuid,
        workflow_id -> Uuid,
        status -> Varchar,
        render_spec_hash -> Varchar,
        storage_key -> Nullable<Text>,
        public_url -> Nullable<Text>,
        duration_ms -> Nullable<Int8>,
        width -> Int4,
        height -> Int4,
        fps -> Int4,
        error_message -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::allow_tables_to_appear_in_same_query!(
    projects,
    project_versions,
    scenes,
    assets,
    generation_tasks,
    renders,
);
