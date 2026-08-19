CREATE TABLE projects (
    id UUID PRIMARY KEY,
    title TEXT NOT NULL,
    source_text TEXT NOT NULL,
    language VARCHAR(16) NOT NULL,
    aspect_ratio VARCHAR(16) NOT NULL,
    target_duration_seconds INTEGER NOT NULL,
    text_provider VARCHAR(64) NOT NULL,
    text_model VARCHAR(128) NOT NULL,
    image_provider VARCHAR(64) NOT NULL,
    image_model VARCHAR(128) NOT NULL,
    voice VARCHAR(64) NOT NULL,
    speech_provider VARCHAR(64) NOT NULL,
    speech_model VARCHAR(128) NOT NULL,
    alignment_provider VARCHAR(64) NOT NULL,
    alignment_model VARCHAR(128) NOT NULL,
    require_script_review BOOLEAN NOT NULL DEFAULT TRUE,
    status VARCHAR(32) NOT NULL DEFAULT 'draft',
    active_workflow_id UUID,
    current_version INTEGER NOT NULL DEFAULT 0,
    error_message TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CHECK (target_duration_seconds BETWEEN 10 AND 3600),
    CHECK (aspect_ratio IN ('16:9', '9:16', '1:1')),
    CHECK (text_provider <> ''),
    CHECK (text_model <> ''),
    CHECK (image_provider <> ''),
    CHECK (image_model <> ''),
    CHECK (speech_provider <> ''),
    CHECK (speech_model <> ''),
    CHECK (alignment_provider <> ''),
    CHECK (alignment_model <> ''),
    CHECK (status IN ('draft', 'queued', 'generating_script', 'waiting_script_review', 'generating_storyboard', 'generating_assets', 'building_timeline', 'rendering', 'completed', 'failed'))
);
CREATE INDEX idx_projects_status_updated_at ON projects (status, updated_at DESC);
CREATE INDEX idx_projects_active_workflow ON projects (active_workflow_id) WHERE active_workflow_id IS NOT NULL;

CREATE TABLE project_versions (
    id UUID PRIMARY KEY,
    project_id UUID NOT NULL,
    version INTEGER NOT NULL,
    script_spec JSONB,
    storyboard_spec JSONB,
    render_spec JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (project_id, version)
);
CREATE INDEX idx_project_versions_project ON project_versions (project_id, version DESC);

CREATE TABLE scenes (
    id UUID PRIMARY KEY,
    project_id UUID NOT NULL,
    project_version_id UUID NOT NULL,
    sequence INTEGER NOT NULL,
    narration_text TEXT NOT NULL,
    visual_prompt TEXT NOT NULL,
    on_screen_text TEXT,
    transition VARCHAR(16) NOT NULL,
    status VARCHAR(32) NOT NULL DEFAULT 'pending',
    duration_ms BIGINT,
    image_asset_id UUID,
    audio_asset_id UUID,
    caption_asset_id UUID,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (project_version_id, sequence),
    CHECK (status IN ('pending', 'generating', 'ready', 'failed')),
    CHECK (transition IN ('fade', 'slide', 'wipe', 'none'))
);
CREATE INDEX idx_scenes_project_version ON scenes (project_version_id, sequence);

CREATE TABLE assets (
    id UUID PRIMARY KEY,
    project_id UUID NOT NULL,
    scene_id UUID,
    kind VARCHAR(32) NOT NULL,
    provider VARCHAR(64) NOT NULL,
    model VARCHAR(128) NOT NULL,
    storage_key TEXT NOT NULL,
    public_url TEXT NOT NULL,
    content_type VARCHAR(128) NOT NULL,
    byte_size BIGINT NOT NULL,
    checksum VARCHAR(128) NOT NULL,
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (storage_key)
);
CREATE INDEX idx_assets_project_scene ON assets (project_id, scene_id, kind);

CREATE TABLE generation_tasks (
    id UUID PRIMARY KEY,
    workflow_id UUID NOT NULL,
    project_id UUID NOT NULL,
    project_version_id UUID,
    scene_id UUID,
    stage VARCHAR(64) NOT NULL,
    status VARCHAR(32) NOT NULL DEFAULT 'pending',
    attempt INTEGER NOT NULL DEFAULT 0,
    idempotency_key VARCHAR(256) NOT NULL,
    provider VARCHAR(64),
    model VARCHAR(128),
    output JSONB,
    usage JSONB,
    error_message TEXT,
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (idempotency_key),
    CHECK (status IN ('pending', 'running', 'succeeded', 'failed'))
);
CREATE INDEX idx_generation_tasks_workflow ON generation_tasks (workflow_id, created_at);
CREATE INDEX idx_generation_tasks_project_stage ON generation_tasks (project_id, stage, status);

CREATE TABLE renders (
    id UUID PRIMARY KEY,
    project_id UUID NOT NULL,
    project_version_id UUID NOT NULL,
    workflow_id UUID NOT NULL,
    status VARCHAR(32) NOT NULL DEFAULT 'queued',
    render_spec_hash VARCHAR(128) NOT NULL,
    storage_key TEXT,
    public_url TEXT,
    duration_ms BIGINT,
    width INTEGER NOT NULL,
    height INTEGER NOT NULL,
    fps INTEGER NOT NULL,
    error_message TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (project_version_id, render_spec_hash),
    CHECK (status IN ('queued', 'rendering', 'completed', 'failed'))
);
CREATE INDEX idx_renders_project ON renders (project_id, created_at DESC);
