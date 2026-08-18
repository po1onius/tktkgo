import { execFile } from "node:child_process";
import { mkdir, rename, rm } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { bundle } from "@remotion/bundler";
import { renderMedia, selectComposition } from "@remotion/renderer";
import Fastify from "fastify";
import pLimit from "p-limit";
import { renderJobSchema, type RenderJob } from "./types";

const currentDirectory = path.dirname(fileURLToPath(import.meta.url));
const assetRoot = path.resolve(
  process.env.TKTKGO_ASSET_ROOT ?? path.join(currentDirectory, "../../storage"),
);
const publicAssetBaseUrl = (
  process.env.TKTKGO_PUBLIC_ASSET_BASE_URL ?? "http://localhost:8000/assets"
).replace(/\/$/, "");
const port = Number.parseInt(process.env.TKTKGO_RENDER_PORT ?? "8090", 10);
const renderLimit = pLimit(
  Number.parseInt(process.env.TKTKGO_RENDER_CONCURRENCY ?? "2", 10),
);
const execFileAsync = promisify(execFile);
type RenderResult = {
  storage_key: string;
  public_url: string;
  duration_ms: number;
};
// 同一输出键的并发请求共享一个 Promise，避免 Restate 重试与原请求同时重复渲染。
const activeRenders = new Map<string, Promise<RenderResult>>();

const app = Fastify({
  logger: { level: process.env.TKTKGO_RENDER_LOG ?? "info" },
  bodyLimit: 10 * 1024 * 1024,
});
const serveUrlPromise = bundle({
  entryPoint: path.join(currentDirectory, "index.tsx"),
});

app.get("/health", async () => ({ status: "ok", service: "render" }));

app.post("/renders", async (request, reply) => {
  const parsed = renderJobSchema.safeParse(request.body);
  if (!parsed.success) {
    request.log.warn({ issues: parsed.error.issues }, "RenderSpec 校验失败");
    return reply
      .code(400)
      .send({ code: "invalid_render_spec", message: parsed.error.message });
  }

  try {
    const result = await renderLimit(() =>
      executeRender(parsed.data, request.log),
    );
    return reply.code(200).send(result);
  } catch (error) {
    request.log.error(
      {
        err: error,
        renderId: parsed.data.render_id,
        projectId: parsed.data.spec.project_id,
      },
      "Remotion 渲染失败",
    );
    return reply.code(500).send({
      code: "render_failed",
      message: error instanceof Error ? error.message : String(error),
    });
  }
});

async function executeRender(job: RenderJob, log: typeof app.log) {
  const outputPath = resolveSafeOutput(job.output_key);
  const active = activeRenders.get(outputPath);
  if (active) {
    log.info(
      { renderId: job.render_id, outputPath },
      "合并到正在执行的同一渲染任务",
    );
    return active;
  }
  const running = executeRenderOnce(job, log, outputPath).finally(() => {
    activeRenders.delete(outputPath);
  });
  activeRenders.set(outputPath, running);
  return running;
}

async function executeRenderOnce(
  job: RenderJob,
  log: typeof app.log,
  outputPath: string,
): Promise<RenderResult> {
  await mkdir(path.dirname(outputPath), { recursive: true });
  const expectedDurationMs = Math.ceil(
    (job.spec.scenes.reduce(
      (max, scene) =>
        Math.max(max, scene.start_frame + scene.duration_in_frames),
      0,
    ) /
      job.spec.fps) *
      1000,
  );

  let durationMs = await probeVideoDurationMs(outputPath, log);
  if (
    durationMs !== null &&
    !durationMatchesExpected(durationMs, expectedDurationMs)
  ) {
    log.warn(
      { renderId: job.render_id, outputPath, durationMs, expectedDurationMs },
      "已有渲染产物时长不匹配，将重新渲染",
    );
    durationMs = null;
  }
  if (durationMs === null) {
    // 永远不直接写最终路径：只有 ffprobe 验证通过的完整 MP4 才能原子替换正式产物。
    const temporaryPath = `${outputPath}.${job.render_id}.partial.mp4`;
    await rm(temporaryPath, { force: true });
    const serveUrl = await serveUrlPromise;
    const inputProps = { spec: job.spec };
    const composition = await selectComposition({
      serveUrl,
      id: "GeneratedVideo",
      inputProps,
    });
    log.info(
      {
        renderId: job.render_id,
        projectId: job.spec.project_id,
        outputPath,
        expectedDurationMs,
      },
      "开始 Remotion 渲染",
    );
    try {
      await renderMedia({
        composition,
        serveUrl,
        codec: "h264",
        audioCodec: "aac",
        outputLocation: temporaryPath,
        inputProps,
        concurrency: 2,
        onProgress: ({ progress }) =>
          log.info(
            { renderId: job.render_id, progress: Number(progress.toFixed(3)) },
            "Remotion 渲染进度",
          ),
      });
      durationMs = await probeVideoDurationMs(temporaryPath, log);
      if (durationMs === null)
        throw new Error("Remotion 输出无法通过 ffprobe 校验");
      if (!durationMatchesExpected(durationMs, expectedDurationMs)) {
        throw new Error(
          `Remotion 输出时长不匹配：实际 ${durationMs}ms，预期 ${expectedDurationMs}ms`,
        );
      }
      await rename(temporaryPath, outputPath);
    } catch (error) {
      await rm(temporaryPath, { force: true });
      throw error;
    }
  } else {
    log.info(
      { renderId: job.render_id, outputPath, durationMs },
      "命中已验证的渲染产物",
    );
  }

  return {
    storage_key: job.output_key,
    public_url: `${publicAssetBaseUrl}/${job.output_key}`,
    duration_ms: durationMs,
  };
}

async function probeVideoDurationMs(
  videoPath: string,
  log: typeof app.log,
): Promise<number | null> {
  try {
    const { stdout } = await execFileAsync("ffprobe", [
      "-v",
      "error",
      "-show_entries",
      "stream=codec_type:format=duration",
      "-of",
      "json",
      videoPath,
    ]);
    const probe = JSON.parse(stdout) as {
      streams?: Array<{ codec_type?: string }>;
      format?: { duration?: string };
    };
    if (!probe.streams?.some((stream) => stream.codec_type === "video")) {
      return null;
    }
    const seconds = Number.parseFloat(probe.format?.duration ?? "");
    return Number.isFinite(seconds) && seconds > 0
      ? Math.ceil(seconds * 1000)
      : null;
  } catch (error) {
    log.warn({ err: error, videoPath }, "渲染文件不存在或未通过 ffprobe 校验");
    return null;
  }
}

function durationMatchesExpected(
  actualMs: number,
  expectedMs: number,
): boolean {
  // 容忍编码器尾帧和音频采样造成的小偏差，但拒绝可播放却明显截断的缓存文件。
  const toleranceMs = Math.max(1000, Math.ceil(expectedMs * 0.02));
  return Math.abs(actualMs - expectedMs) <= toleranceMs;
}

function resolveSafeOutput(key: string): string {
  if (path.isAbsolute(key) || key.includes("..") || !key.endsWith(".mp4"))
    throw new Error("output_key 必须是安全的 mp4 相对路径");
  const resolved = path.resolve(assetRoot, key);
  if (!resolved.startsWith(`${assetRoot}${path.sep}`))
    throw new Error("output_key 超出素材目录");
  return resolved;
}

await app.listen({ host: "0.0.0.0", port });
