import { Audio } from "@remotion/media";
import React from "react";
import {
  AbsoluteFill,
  Img,
  Sequence,
  interpolate,
  spring,
  useCurrentFrame,
  useVideoConfig,
} from "remotion";
import { RENDER_FONT_FAMILY, useRenderFonts } from "./fonts";
import type { CaptionCue, RenderScene, RenderSpec } from "./types";

export const GeneratedVideo: React.FC<{ spec: RenderSpec }> = ({ spec }) => {
  // Fontsource 的中文字体按 Unicode Range 分包；把本次视频所有会展示的文字交给
  // 字体加载器，确保涉及的 WOFF2 子集在 Remotion 捕获首帧之前全部准备完成。
  const renderText = spec.scenes
    .flatMap((scene) => [
      scene.on_screen_text ?? "",
      ...scene.captions.map((cue) => cue.text),
    ])
    .join("");
  useRenderFonts(renderText);

  return (
    <AbsoluteFill
      style={{
        backgroundColor: spec.background_color,
        fontFamily: RENDER_FONT_FAMILY,
      }}
    >
      {spec.scenes.map((scene) => (
        <Sequence
          key={scene.id}
          from={scene.start_frame}
          durationInFrames={scene.duration_in_frames}
          premountFor={spec.fps}
        >
          <SceneView scene={scene} />
        </Sequence>
      ))}
    </AbsoluteFill>
  );
};

const SceneView: React.FC<{ scene: RenderScene }> = ({ scene }) => {
  const frame = useCurrentFrame();
  const { fps, width } = useVideoConfig();
  const entrance = spring({ frame, fps, config: { damping: 18, mass: 0.8 } });
  const scale = interpolate(
    frame,
    [0, scene.duration_in_frames],
    [1.03, 1.11],
    {
      extrapolateLeft: "clamp",
      extrapolateRight: "clamp",
    },
  );
  const opacity = interpolate(
    frame,
    [
      0,
      Math.round(fps * 0.35),
      scene.duration_in_frames - Math.round(fps * 0.3),
      scene.duration_in_frames,
    ],
    [0, 1, 1, 0],
    {
      extrapolateLeft: "clamp",
      extrapolateRight: "clamp",
    },
  );
  const transitionStyle: React.CSSProperties = (() => {
    switch (scene.transition) {
      case "none":
        return {};
      case "slide":
        return {
          opacity,
          transform: `translateX(${(1 - entrance) * 8}%)`,
        };
      case "wipe":
        return {
          opacity,
          clipPath: `inset(0 ${(1 - entrance) * 100}% 0 0)`,
        };
      default:
        return { opacity };
    }
  })();

  return (
    <AbsoluteFill style={{ overflow: "hidden", ...transitionStyle }}>
      <Img
        src={scene.image_url}
        style={{
          width: "100%",
          height: "100%",
          objectFit: "cover",
          transform: `scale(${scale})`,
        }}
      />
      <AbsoluteFill
        style={{
          background:
            "linear-gradient(180deg, rgba(4,8,20,0.06) 35%, rgba(4,8,20,0.82) 100%)",
        }}
      />
      {scene.on_screen_text ? (
        <div
          style={{
            position: "absolute",
            left: "7%",
            right: "7%",
            top: "11%",
            color: "white",
            fontSize: Math.round(width * 0.046),
            fontWeight: 800,
            lineHeight: 1.18,
            textShadow: "0 4px 24px rgba(0,0,0,.5)",
            transform: `translateY(${(1 - entrance) * 40}px)`,
            opacity: entrance,
          }}
        >
          {scene.on_screen_text}
        </div>
      ) : null}
      <CaptionLine cues={scene.captions} />
      <Audio src={scene.audio_url} />
    </AbsoluteFill>
  );
};

const CaptionLine: React.FC<{ cues: CaptionCue[] }> = ({ cues }) => {
  const frame = useCurrentFrame();
  const { fps, width } = useVideoConfig();
  const currentMs = (frame / fps) * 1000;
  const activeIndex = cues.findIndex(
    (cue) => currentMs >= cue.start_ms && currentMs < cue.end_ms,
  );
  if (activeIndex < 0) return null;

  // 以当前词为中心展示一个短窗口，避免中文单词时间戳造成字幕逐字跳动。
  const start = Math.max(0, activeIndex - 3);
  const end = Math.min(cues.length, activeIndex + 5);
  return (
    <div
      style={{
        position: "absolute",
        left: "8%",
        right: "8%",
        bottom: "8%",
        textAlign: "center",
        color: "white",
        fontSize: Math.round(width * 0.035),
        fontWeight: 700,
        lineHeight: 1.35,
        textShadow: "0 3px 12px rgba(0,0,0,.95)",
      }}
    >
      {cues.slice(start, end).map((cue, offset) => {
        const index = start + offset;
        return (
          <span
            key={`${cue.start_ms}-${index}`}
            style={{ color: index === activeIndex ? "#FFD166" : "white" }}
          >
            {cue.text}
          </span>
        );
      })}
    </div>
  );
};
