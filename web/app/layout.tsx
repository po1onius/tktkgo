import type { Metadata } from "next";
import "./styles.css";

export const metadata: Metadata = {
  title: "TK视频工坊",
  description: "从主题到可编辑视频",
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
