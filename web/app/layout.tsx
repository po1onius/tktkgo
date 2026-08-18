import type { Metadata } from "next";
import "./styles.css";

export const metadata: Metadata = {
  title: "TK视频工坊",
  description: "从主题到可审核、可重复生成的视频",
};

export default function RootLayout({
  children,
}: Readonly<{ children: React.ReactNode }>) {
  return (
    <html lang="zh-CN">
      <body>{children}</body>
    </html>
  );
}
