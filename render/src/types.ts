import { z } from "zod";

export const captionCueSchema = z
  .object({
    text: z.string().trim().min(1),
    start_ms: z.number().int().nonnegative(),
    end_ms: z.number().int().positive(),
  })
  .refine((cue) => cue.end_ms > cue.start_ms, {
    message: "字幕结束时间必须晚于开始时间",
    path: ["end_ms"],
  });

export const renderSceneSchema = z.object({
  id: z.string().uuid(),
  sequence: z.number().int().positive(),
  start_frame: z.number().int().nonnegative(),
  duration_in_frames: z.number().int().positive(),
  image_url: z.url(),
  audio_url: z.url(),
  on_screen_text: z.string().nullable(),
  transition: z.enum(["fade", "slide", "wipe", "none"]),
  captions: z.array(captionCueSchema),
});

export const renderSpecSchema = z
  .object({
    project_id: z.string().uuid(),
    version: z.number().int().positive(),
    width: z.number().int().min(320).max(7680),
    height: z.number().int().min(320).max(7680),
    fps: z.number().int().min(24).max(60),
    background_color: z.string().regex(/^#[0-9a-fA-F]{6}$/),
    scenes: z.array(renderSceneSchema).min(1).max(300),
  })
  .superRefine((spec, context) => {
    // 渲染服务是协议边界，不能只相信 Rust 已经做过的业务校验。
    let expectedStartFrame = 0;
    spec.scenes.forEach((scene, sceneIndex) => {
      if (scene.sequence !== sceneIndex + 1) {
        context.addIssue({
          code: "custom",
          message: "场景 sequence 必须从 1 连续递增",
          path: ["scenes", sceneIndex, "sequence"],
        });
      }
      if (scene.start_frame !== expectedStartFrame) {
        context.addIssue({
          code: "custom",
          message: "场景时间线必须连续且不能重叠",
          path: ["scenes", sceneIndex, "start_frame"],
        });
      }
      const sceneDurationMs = Math.ceil(
        (scene.duration_in_frames / spec.fps) * 1000,
      );
      let previousEndMs = 0;
      scene.captions.forEach((cue, cueIndex) => {
        if (
          cue.start_ms < previousEndMs ||
          cue.end_ms > sceneDurationMs + 1000
        ) {
          context.addIssue({
            code: "custom",
            message: "字幕必须按时间排序且不能超出场景时长",
            path: ["scenes", sceneIndex, "captions", cueIndex],
          });
        }
        previousEndMs = cue.end_ms;
      });
      expectedStartFrame += scene.duration_in_frames;
    });
  });

export const renderJobSchema = z.object({
  render_id: z.string().uuid(),
  output_key: z.string().min(1).max(500),
  spec: renderSpecSchema,
});

export type CaptionCue = z.infer<typeof captionCueSchema>;
export type RenderScene = z.infer<typeof renderSceneSchema>;
export type RenderSpec = z.infer<typeof renderSpecSchema>;
export type RenderJob = z.infer<typeof renderJobSchema>;
