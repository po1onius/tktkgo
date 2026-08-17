import { z } from "zod";

export const captionCueSchema = z.object({
  text: z.string().min(1),
  start_ms: z.number().int().nonnegative(),
  end_ms: z.number().int().positive(),
});

export const renderSceneSchema = z.object({
  id: z.string().uuid(),
  sequence: z.number().int().positive(),
  start_frame: z.number().int().nonnegative(),
  duration_in_frames: z.number().int().positive(),
  narration: z.string().min(1),
  visual_type: z.enum([
    "illustration",
    "infographic",
    "quote",
    "title",
    "list",
  ]),
  image_url: z.url(),
  audio_url: z.url(),
  on_screen_text: z.string().nullable(),
  transition: z.enum(["fade", "slide", "wipe", "none"]),
  captions: z.array(captionCueSchema),
});

export const renderSpecSchema = z.object({
  project_id: z.string().uuid(),
  version: z.number().int().positive(),
  width: z.number().int().min(320).max(7680),
  height: z.number().int().min(320).max(7680),
  fps: z.number().int().min(24).max(60),
  background_color: z.string().regex(/^#[0-9a-fA-F]{6}$/),
  scenes: z.array(renderSceneSchema).min(1).max(300),
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
