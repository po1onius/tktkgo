import "@fontsource-variable/noto-sans-sc";
import { useEffect, useState } from "react";
import { useDelayRender } from "remotion";

const FONT_FACE_FAMILY = '"Noto Sans SC Variable"';

/**
 * 视频模板统一使用随 npm 依赖打包的简体中文可变字体。
 * 不依赖宿主机 fontconfig，确保 NixOS、容器和其他部署环境渲染出相同字形。
 */
export const RENDER_FONT_FAMILY = `${FONT_FACE_FAMILY}, sans-serif`;

/**
 * Fontsource 将 CJK 字体按 Unicode Range 拆成多个 WOFF2 文件。这里使用本次视频的
 * 全部文字显式触发所需子集下载，并通过 delayRender 阻止 Remotion 在字体加载完成前
 * 捕获任何帧，避免中文偶发回退为方框或不同机器得到不同排版。
 */
export function useRenderFonts(text: string): void {
  const { cancelRender, continueRender, delayRender } = useDelayRender();
  const [handle] = useState(() =>
    delayRender("正在加载视频中文字体", {
      timeoutInMilliseconds: 60_000,
    }),
  );

  useEffect(() => {
    let disposed = false;
    const sample = text.trim() || "视频字幕字体加载验证";

    Promise.all([
      document.fonts.load(`700 48px ${FONT_FACE_FAMILY}`, sample),
      document.fonts.load(`800 48px ${FONT_FACE_FAMILY}`, sample),
    ])
      .then(async (loadedFaces) => {
        if (loadedFaces.some((faces) => faces.length === 0)) {
          throw new Error("Noto Sans SC 字体没有匹配到可用字形");
        }
        await document.fonts.ready;
        if (disposed) return;
        console.info("[render-fonts] 视频中文字体加载完成", {
          family: FONT_FACE_FAMILY,
          textLength: sample.length,
        });
        continueRender(handle);
      })
      .catch((error: unknown) => {
        if (disposed) return;
        cancelRender(
          error instanceof Error
            ? error
            : new Error(`加载视频中文字体失败: ${String(error)}`),
        );
      });

    return () => {
      disposed = true;
      // 组件在字体 Promise 完成前卸载时必须释放句柄，避免预览或重新选取 Composition
      // 留下无法完成的 delayRender；已释放的句柄再次 continueRender 是安全的。
      continueRender(handle);
    };
  }, [cancelRender, continueRender, handle, text]);
}
