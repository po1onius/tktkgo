import type { NextConfig } from "next";
import { PHASE_DEVELOPMENT_SERVER } from "next/constants";

const nextConfig = (phase: string): NextConfig => {
  const apiBase = (
    process.env.TKTKGO_WEB_DEV_API_URL ?? "http://127.0.0.1:8000"
  ).replace(/\/$/, "");

  return {
    // Web 只使用浏览器端 React 能力，导出为纯静态文件后交给 Rust API 统一托管。
    // 这样生产运行时不需要额外启动 Next.js 服务，也不会产生跨域请求。
    output: "export",
    reactStrictMode: true,
    // 单独执行 `pnpm --filter @tktkgo/web dev` 时仍保留热更新体验。
    // 代理只在 Next.js 开发服务器阶段启用，不会进入静态构建或生产运行时。
    ...(phase === PHASE_DEVELOPMENT_SERVER
      ? {
          async rewrites() {
            return [
              {
                source: "/v1/:path*",
                destination: `${apiBase}/v1/:path*`,
              },
              {
                source: "/assets/:path*",
                destination: `${apiBase}/assets/:path*`,
              },
            ];
          },
        }
      : {}),
  };
};

export default nextConfig;
