import { existsSync } from "node:fs";
import { mkdir } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
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
  await mkdir(path.dirname(outputPath), { recursive: true });
  const durationMs = Math.ceil(
    (job.spec.scenes.reduce(
      (max, scene) =>
        Math.max(max, scene.start_frame + scene.duration_in_frames),
      0,
    ) /
      job.spec.fps) *
      1000,
  );

  if (!existsSync(outputPath)) {
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
        durationMs,
      },
      "开始 Remotion 渲染",
    );
    await renderMedia({
      composition,
      serveUrl,
      codec: "h264",
      audioCodec: "aac",
      outputLocation: outputPath,
      inputProps,
      concurrency: 2,
      onProgress: ({ progress }) =>
        log.info(
          { renderId: job.render_id, progress: Number(progress.toFixed(3)) },
          "Remotion 渲染进度",
        ),
    });
  } else {
    log.info({ renderId: job.render_id, outputPath }, "命中已有渲染产物");
  }

  return {
    storage_key: job.output_key,
    public_url: `${publicAssetBaseUrl}/${job.output_key}`,
    duration_ms: durationMs,
  };
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
