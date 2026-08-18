"use client";

import { FormEvent, useEffect, useState } from "react";

const apiBase = process.env.NEXT_PUBLIC_API_BASE_URL ?? "http://localhost:8000";

type Project = {
  id: string;
  title: string;
  source_text: string;
  language: string;
  aspect_ratio: string;
  target_duration_seconds: number;
  voice: string;
  speech_provider: string;
  speech_model: string;
  transcription_provider: string;
  transcription_model: string;
  require_script_review: boolean;
  status: string;
  current_version: number;
  error_message: string | null;
};

type ProjectVersion = {
  version: number;
  script_spec: { full_narration?: string; summary?: string } | null;
};
type RenderRecord = {
  status: string;
  public_url: string | null;
  duration_ms: number | null;
};

type ProviderOption = {
  id: string;
  label: string;
  model: string;
  available: boolean;
  voices: string[];
};

type ProviderCatalog = {
  speech: ProviderOption[];
  transcription: ProviderOption[];
};

const fallbackProviders: ProviderCatalog = {
  speech: [
    {
      id: "openai",
      label: "OpenAI",
      model: "gpt-4o-mini-tts",
      available: true,
      voices: ["coral", "alloy", "sage"],
    },
  ],
  transcription: [
    {
      id: "openai",
      label: "OpenAI",
      model: "whisper-1",
      available: true,
      voices: [],
    },
  ],
};

const statusLabel: Record<string, string> = {
  draft: "草稿",
  queued: "已排队",
  generating_script: "正在生成文稿",
  waiting_script_review: "等待文稿审核",
  generating_storyboard: "正在设计分镜",
  generating_assets: "正在生成插图与口播",
  building_timeline: "正在构建时间轴",
  rendering: "正在渲染视频",
  completed: "已完成",
  failed: "失败",
};

export default function Home() {
  const [project, setProject] = useState<Project | null>(null);
  const [version, setVersion] = useState<ProjectVersion | null>(null);
  const [render, setRender] = useState<RenderRecord | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [providers, setProviders] =
    useState<ProviderCatalog>(fallbackProviders);
  const [speechProvider, setSpeechProvider] = useState("openai");
  const [transcriptionProvider, setTranscriptionProvider] = useState("openai");
  const [voice, setVoice] = useState("coral");

  useEffect(() => {
    void request<ProviderCatalog>("/v1/providers")
      .then((catalog) => {
        setProviders(catalog);
        console.info("provider catalog loaded", { catalog });
      })
      .catch((cause) => {
        // Provider 目录读取失败不妨碍使用默认 OpenAI，但保留日志用于排查本地服务。
        console.error("load provider catalog failed", { cause });
      });
  }, []);

  const selectedSpeech =
    providers.speech.find((provider) => provider.id === speechProvider) ??
    providers.speech[0];
  const voices = selectedSpeech?.voices.length
    ? selectedSpeech.voices
    : fallbackProviders.speech[0].voices;
  const selectedTranscription =
    providers.transcription.find(
      (provider) => provider.id === transcriptionProvider,
    ) ?? providers.transcription[0];

  useEffect(() => {
    if (!voices.includes(voice)) setVoice(voices[0]);
  }, [speechProvider, providers, voice, voices]);

  useEffect(() => {
    if (!project || ["completed", "failed", "draft"].includes(project.status))
      return;
    const timer = window.setInterval(() => void refresh(project.id), 2500);
    return () => window.clearInterval(timer);
  }, [project?.id, project?.status]);

  async function refresh(projectId: string) {
    try {
      const next = await request<Project>(`/v1/projects/${projectId}`);
      setProject(next);
      if (next.current_version > 0)
        setVersion(
          await request<ProjectVersion | null>(
            `/v1/projects/${projectId}/version`,
          ),
        );
      if (next.status === "completed")
        setRender(
          await request<RenderRecord | null>(
            `/v1/projects/${projectId}/renders/latest`,
          ),
        );
      console.info("project refreshed", {
        projectId,
        status: next.status,
        version: next.current_version,
      });
    } catch (cause) {
      console.error("refresh project failed", { projectId, cause });
      setError(messageOf(cause));
    }
  }

  async function create(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setBusy(true);
    setError(null);
    const data = new FormData(event.currentTarget);
    try {
      const created = await request<Project>("/v1/projects", {
        method: "POST",
        body: JSON.stringify({
          title: data.get("title"),
          source_text: data.get("sourceText"),
          language: "zh-CN",
          aspect_ratio: data.get("aspectRatio"),
          target_duration_seconds: Number(data.get("duration")),
          voice: data.get("voice"),
          speech_provider: speechProvider,
          speech_model: selectedSpeech.model,
          transcription_provider: transcriptionProvider,
          transcription_model: selectedTranscription.model,
          require_script_review: true,
          auto_start: true,
        }),
      });
      console.info("project created", { projectId: created.id });
      setProject(created);
    } catch (cause) {
      console.error("create project failed", { cause });
      setError(messageOf(cause));
    } finally {
      setBusy(false);
    }
  }

  async function review(approved: boolean) {
    if (!project) return;
    setBusy(true);
    setError(null);
    try {
      await request(`/v1/projects/${project.id}/script-review`, {
        method: "POST",
        body: JSON.stringify({
          approved,
          feedback: approved ? null : "请重新调整文稿后发起新版本",
        }),
      });
      console.info("script review submitted", {
        projectId: project.id,
        approved,
      });
      await refresh(project.id);
    } catch (cause) {
      console.error("script review failed", { projectId: project.id, cause });
      setError(messageOf(cause));
    } finally {
      setBusy(false);
    }
  }

  return (
    <main>
      <header>
        <div className="eyebrow">AI VIDEO STUDIO</div>
        <h1>把一个想法，变成一条完整视频。</h1>
        <p>
          文稿、分镜、插图、口播与字幕由工作流逐步生成，每一步都可以追踪和重放。
        </p>
      </header>

      {!project ? (
        <form className="panel form" onSubmit={create}>
          <label>
            视频标题
            <input
              name="title"
              required
              maxLength={200}
              placeholder="例如：为什么我们会做梦？"
            />
          </label>
          <label>
            主题或大纲
            <textarea
              name="sourceText"
              required
              maxLength={20000}
              rows={8}
              placeholder="写下一句话、几条要点，或者直接粘贴大纲……"
            />
          </label>
          <div className="row">
            <label>
              画幅
              <select name="aspectRatio" defaultValue="landscape">
                <option value="landscape">横屏 16:9</option>
                <option value="portrait">竖屏 9:16</option>
                <option value="square">方形 1:1</option>
              </select>
            </label>
            <label>
              目标时长
              <select name="duration" defaultValue="180">
                <option value="60">1 分钟</option>
                <option value="180">3 分钟</option>
                <option value="300">5 分钟</option>
              </select>
            </label>
            <label>
              口播声音
              <select
                name="voice"
                value={voice}
                onChange={(event) => setVoice(event.target.value)}
              >
                {voices.map((item) => (
                  <option key={item} value={item}>
                    {item}
                  </option>
                ))}
              </select>
            </label>
          </div>
          <fieldset className="provider-settings">
            <legend>音频模型</legend>
            <div className="provider-row">
              <label>
                口播 Provider
                <select
                  name="speechProvider"
                  value={speechProvider}
                  onChange={(event) => setSpeechProvider(event.target.value)}
                >
                  {providers.speech.map((provider) => (
                    <option
                      key={provider.id}
                      value={provider.id}
                      disabled={!provider.available}
                    >
                      {provider.label} · {provider.model}
                      {provider.available ? "" : "（未启动）"}
                    </option>
                  ))}
                </select>
              </label>
              <label>
                字幕 Provider
                <select
                  name="transcriptionProvider"
                  value={transcriptionProvider}
                  onChange={(event) =>
                    setTranscriptionProvider(event.target.value)
                  }
                >
                  {providers.transcription.map((provider) => (
                    <option
                      key={provider.id}
                      value={provider.id}
                      disabled={!provider.available}
                    >
                      {provider.label} · {provider.model}
                      {provider.available ? "" : "（未启动）"}
                    </option>
                  ))}
                </select>
              </label>
            </div>
            <p>
              本地模型在第一次实际生成时才加载；未启动的 Provider 不可选择。
            </p>
          </fieldset>
          <button disabled={busy}>{busy ? "正在创建…" : "开始生成"}</button>
        </form>
      ) : (
        <section className="panel progress">
          <div className="project-head">
            <div>
              <span className={`status ${project.status}`}>
                {statusLabel[project.status] ?? project.status}
              </span>
              <h2>{project.title}</h2>
            </div>
            <button
              className="secondary"
              onClick={() => {
                setProject(null);
                setVersion(null);
                setRender(null);
              }}
            >
              新建项目
            </button>
          </div>
          <div className="meta">
            <span>ID {project.id}</span>
            <span>版本 {project.current_version}</span>
            <span>{project.aspect_ratio}</span>
            <span>{project.target_duration_seconds} 秒</span>
            <span>
              口播 {project.speech_provider}/{project.speech_model}
            </span>
            <span>
              字幕 {project.transcription_provider}/
              {project.transcription_model}
            </span>
          </div>
          {project.error_message ? (
            <div className="notice error">{project.error_message}</div>
          ) : null}

          {project.status === "waiting_script_review" ? (
            <div className="script-review">
              <h3>审核口播稿</h3>
              <p>{version?.script_spec?.summary}</p>
              <article>
                {version?.script_spec?.full_narration ?? "正在读取文稿……"}
              </article>
              <div className="actions">
                <button disabled={busy} onClick={() => void review(true)}>
                  通过并继续
                </button>
                <button
                  disabled={busy}
                  className="danger"
                  onClick={() => void review(false)}
                >
                  退回
                </button>
              </div>
            </div>
          ) : null}

          {project.status === "completed" && render?.public_url ? (
            <div className="result">
              <video src={render.public_url} controls />
              <a className="download" href={render.public_url} download>
                下载 MP4
              </a>
            </div>
          ) : null}

          {!["waiting_script_review", "completed", "failed", "draft"].includes(
            project.status,
          ) ? (
            <div className="activity">
              <span />
              <p>{statusLabel[project.status]}，页面会自动更新。</p>
            </div>
          ) : null}
        </section>
      )}
      {error ? <div className="notice error global">{error}</div> : null}
    </main>
  );
}

async function request<T = unknown>(
  path: string,
  init?: RequestInit,
): Promise<T> {
  const response = await fetch(`${apiBase}${path}`, {
    ...init,
    headers: { "content-type": "application/json", ...init?.headers },
  });
  if (!response.ok) {
    const body = await response
      .json()
      .catch(() => ({ message: response.statusText }));
    throw new Error(body.message ?? `HTTP ${response.status}`);
  }
  if (response.status === 204 || response.headers.get("content-length") === "0")
    return undefined as T;
  return response.json() as Promise<T>;
}

function messageOf(value: unknown) {
  return value instanceof Error ? value.message : String(value);
}
