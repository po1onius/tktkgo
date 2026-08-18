import React from "react";
import { Composition } from "remotion";
import { GeneratedVideo } from "./Video";
import type { RenderSpec } from "./types";

const emptySpec: RenderSpec = {
  project_id: "00000000-0000-4000-8000-000000000000",
  version: 1,
  width: 1920,
  height: 1080,
  fps: 30,
  background_color: "#0B1020",
  scenes: [],
};

export const RemotionRoot: React.FC = () => (
  <Composition
    id="GeneratedVideo"
    component={GeneratedVideo}
    defaultProps={{ spec: emptySpec }}
    width={1920}
    height={1080}
    fps={30}
    durationInFrames={30}
    calculateMetadata={({ props }) => ({
      width: props.spec.width,
      height: props.spec.height,
      fps: props.spec.fps,
      durationInFrames: Math.max(
        1,
        props.spec.scenes.reduce(
          (max, scene) =>
            Math.max(max, scene.start_frame + scene.duration_in_frames),
          0,
        ),
      ),
    })}
  />
);
