use std::path::Path;

use serde::Deserialize;
use tokio::process::Command;
use tracing::{debug, instrument};

use crate::{AppError, AppResult};

#[derive(Debug, Deserialize)]
struct ProbeOutput {
    format: ProbeFormat,
}

#[derive(Debug, Deserialize)]
struct ProbeFormat {
    duration: String,
}

/// 使用成熟的 ffprobe 获取真实媒体时长，不根据文字长度猜测画面帧数。
#[instrument(fields(path = %path.as_ref().display()))]
pub async fn probe_duration_ms(path: impl AsRef<Path>) -> AppResult<i64> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "json",
        ])
        .arg(path.as_ref())
        .output()
        .await
        .map_err(|err| {
            AppError::external(
                "ffprobe",
                format!("无法执行 ffprobe，请手动安装 FFmpeg: {err}"),
            )
        })?;
    if !output.status.success() {
        return Err(AppError::external(
            "ffprobe",
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ));
    }
    let probe: ProbeOutput = serde_json::from_slice(&output.stdout)?;
    let seconds: f64 = probe
        .format
        .duration
        .parse()
        .map_err(|err| AppError::external("ffprobe", format!("无法解析媒体时长: {err}")))?;
    let duration = (seconds * 1000.0).ceil() as i64;
    debug!(duration_ms = duration, "音频时长探测完成");
    Ok(duration)
}
