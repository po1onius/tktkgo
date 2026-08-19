"use client";

import { FormEvent, useEffect, useState } from "react";

// 网页与 API 由同一个 Axum 服务提供，始终使用相对地址继承当前协议、域名和端口。
// 不在静态构建中固化部署地址，避免换域名或通过反向代理访问时产生跨域请求。
const apiBase = "";

type Project = {
  id: string;
  title: string;
  source_text: string;
  language: string;
  aspect_ratio: string;
  target_duration_seconds: number;
  text_provider: string;
  text_model: string;
  image_provider: string;
  image_model: string;
  voice: string;
  speech_provider: string;
  speech_model: string;
  alignment_provider: string;
  alignment_model: string;
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
  text: ProviderOption[];
  image: ProviderOption[];
  speech: ProviderOption[];
  alignment: ProviderOption[];
};

const emptyProviders: ProviderCatalog = {
  text: [],
  image: [],
  speech: [],
  alignment: [],
};

function optionValue(option: ProviderOption): string {
  return `${option.id}::${option.model}`;
}

function firstAvailable(options: ProviderOption[]): string {
  const option = options.find((item) => item.available) ?? options[0];
  return option ? optionValue(option) : "";
}

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
const regeneratableStatuses = new Set(["draft", "failed", "completed"]);

export default function Home() {
  const [project, setProject] = useState<Project | null>(null);
  const [version, setVersion] = useState<ProjectVersion | null>(null);
  const [render, setRender] = useState<RenderRecord | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [providers, setProviders] = useState<ProviderCatalog>(emptyProviders);
  const [textSelection, setTextSelection] = useState("");
  const [imageSelection, setImageSelection] = useState("");
  const [speechSelection, setSpeechSelection] = useState("");
  const [alignmentSelection, setAlignmentSelection] = useState("");
  const [voice, setVoice] = useState("");

  useEffect(() => {
    void request<ProviderCatalog>("/v1/providers")
      .then((catalog) => {
        setProviders(catalog);
        setTextSelection(firstAvailable(catalog.text));
        setImageSelection(firstAvailable(catalog.image));
        setSpeechSelection(firstAvailable(catalog.speech));
        setAlignmentSelection(firstAvailable(catalog.alignment));
        console.info("provider catalog loaded", { catalog });
      })
      .catch((cause) => {
        console.error("load provider catalog failed", { cause });
        setError(`模型网关不可用：${messageOf(cause)}`);
      });
  }, []);

  const selectedSpeech =
    providers.speech.find(
      (provider) => optionValue(provider) === speechSelection,
    ) ?? providers.speech[0];
  const voices = selectedSpeech?.voices ?? [];
  const selectedAlignment =
    providers.alignment.find(
      (provider) => optionValue(provider) === alignmentSelection,
    ) ?? providers.alignment[0];
  const selectedText =
    providers.text.find(
      (provider) => optionValue(provider) === textSelection,
    ) ?? providers.text[0];
  const selectedImage =
    providers.image.find(
      (provider) => optionValue(provider) === imageSelection,
    ) ?? providers.image[0];
  const catalogReady = Boolean(
    selectedText?.available &&
    selectedImage?.available &&
    selectedSpeech?.available &&
    selectedAlignment?.available &&
    voice,
  );

  useEffect(() => {
    if (!voices.length) {
      if (voice) setVoice("");
    } else if (!voices.includes(voice)) {
      setVoice(voices[0]);
    }
  }, [speechSelection, providers, voice, voices]);

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
      if (
        !catalogReady ||
        !selectedText ||
        !selectedImage ||
        !selectedSpeech ||
        !selectedAlignment
      ) {
        throw new Error("模型目录尚未就绪，请先启动 make provider");
      }
      const created = await request<Project>("/v1/projects", {
        method: "POST",
        body: JSON.stringify({
          title: data.get("title"),
          source_text: data.get("sourceText"),
          language: "zh-CN",
          aspect_ratio: data.get("aspectRatio"),
          target_duration_seconds: Number(data.get("duration")),
          text_provider: selectedText.id,
          text_model: selectedText.model,
          image_provider: selectedImage.id,
          image_model: selectedImage.model,
          voice: data.get("voice"),
          speech_provider: selectedSpeech.id,
          speech_model: selectedSpeech.model,
          alignment_provider: selectedAlignment.id,
          alignment_model: selectedAlignment.model,
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

  async function regenerate() {
    if (!project) return;
    setBusy(true);
    setError(null);
    try {
      await request(`/v1/projects/${project.id}/generate`, { method: "POST" });
      setRender(null);
      console.info("project regeneration started", { projectId: project.id });
      await refresh(project.id);
    } catch (cause) {
      console.error("regenerate project failed", {
        projectId: project.id,
        cause,
      });
      setError(messageOf(cause));
      // 派发失败时后端会把项目明确写成 failed，立即刷新以展示可重试状态。
      await refresh(project.id);
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
                disabled={!voices.length}
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
            <legend>模型配置</legend>
            <div className="provider-row">
              <label>
                文案 Provider
                <select
                  name="textProvider"
                  value={textSelection}
                  onChange={(event) => setTextSelection(event.target.value)}
                  disabled={!providers.text.length}
                >
                  {providers.text.map((provider) => (
                    <option
                      key={`${provider.id}/${provider.model}`}
                      value={optionValue(provider)}
                      disabled={!provider.available}
                    >
                      {provider.label} · {provider.model}
                      {provider.available ? "" : "（未启动）"}
                    </option>
                  ))}
                </select>
              </label>
              <label>
                图片 Provider
                <select
                  name="imageProvider"
                  value={imageSelection}
                  onChange={(event) => setImageSelection(event.target.value)}
                  disabled={!providers.image.length}
                >
                  {providers.image.map((provider) => (
                    <option
                      key={`${provider.id}/${provider.model}`}
                      value={optionValue(provider)}
                      disabled={!provider.available}
                    >
                      {provider.label} · {provider.model}
                      {provider.available ? "" : "（未启动）"}
                    </option>
                  ))}
                </select>
              </label>
              <label>
                口播 Provider
                <select
                  name="speechProvider"
                  value={speechSelection}
                  onChange={(event) => setSpeechSelection(event.target.value)}
                  disabled={!providers.speech.length}
                >
                  {providers.speech.map((provider) => (
                    <option
                      key={`${provider.id}/${provider.model}`}
                      value={optionValue(provider)}
                      disabled={!provider.available}
                    >
                      {provider.label} · {provider.model}
                      {provider.available ? "" : "（未启动）"}
                    </option>
                  ))}
                </select>
              </label>
              <label>
                字幕对齐 Provider
                <select
                  name="alignmentProvider"
                  value={alignmentSelection}
                  onChange={(event) =>
                    setAlignmentSelection(event.target.value)
                  }
                  disabled={!providers.alignment.length}
                >
                  {providers.alignment.map((provider) => (
                    <option
                      key={`${provider.id}/${provider.model}`}
                      value={optionValue(provider)}
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
          <button disabled={busy || !catalogReady}>
            {busy ? "正在创建…" : catalogReady ? "开始生成" : "等待模型网关"}
          </button>
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
            <div className="project-head-actions">
              {regeneratableStatuses.has(project.status) ? (
                <button disabled={busy} onClick={() => void regenerate()}>
                  {project.status === "completed" ? "生成新版本" : "重新生成"}
                </button>
              ) : null}
              <button
                className="secondary"
                onClick={() => {
                  setProject(null);
                  setVersion(null);
                  setRender(null);
                  setError(null);
                }}
              >
                新建项目
              </button>
            </div>
          </div>
          <div className="meta">
            <span>ID {project.id}</span>
            <span>版本 {project.current_version}</span>
            <span>{project.aspect_ratio}</span>
            <span>{project.target_duration_seconds} 秒</span>
            <span>
              文案 {project.text_provider}/{project.text_model}
            </span>
            <span>
              图片 {project.image_provider}/{project.image_model}
            </span>
            <span>
              口播 {project.speech_provider}/{project.speech_model}
            </span>
            <span>
              字幕对齐 {project.alignment_provider}/{project.alignment_model}
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
