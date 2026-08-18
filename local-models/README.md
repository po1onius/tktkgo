# 本地模型独立部署

本目录只管理本地模型执行服务，不保存远程 Provider Key，也不启动固定 Provider 网关。两个服务拥有独立配置、Python 环境、模型缓存和进程生命周期。

## faster-whisper

在项目根目录执行：

```bash
make faster-whisper
```

第一次运行会创建 `local-models/faster-whisper/provider.toml`、同步锁定依赖，并把缺失权重下载到该服务的 `models/`。默认使用 `large-v3 + CPU int8`；资源不足可以改用 `small`，NVIDIA CUDA 12 + cuDNN 9 环境建议使用 `cuda + float16`。服务在模型完成加载后才通过健康检查。

## CosyVoice 3

在项目根目录执行：

```bash
make cosyvoice
```

该目标会自动克隆 CosyVoice 官方仓库的已验证提交及其子模块、创建独立 Python 3.10 环境、安装官方依赖，并在服务启动时把缺失的 ModelScope 权重下载到 `local-models/cosyvoice/models/`。上游目前只有 `v2.0` 发布标签，尚无支持 CosyVoice 3 的稳定标签，因此 Makefile 固定了主线提交，避免部署结果随默认分支漂移。

参考音色包含用户自己的语音，无法从公开模型仓库自动获得。第一次运行会创建 `voices.json` 并停止；请把干净的单人参考 WAV 放入 `voices/`，确保 `prompt_text` 与录音内容完全一致，然后重新执行命令。模型与服务参数位于独立的 `local-models/cosyvoice/provider.toml`。

## 与网关连接

本地服务默认分别监听 `http://127.0.0.1:8102` 和 `http://127.0.0.1:8101`。在 `provider/providers.toml` 中配置相同 `base_url`，并将对应模型加入 `models.transcription.faster_whisper` 或 `models.speech.cosy_voice`。网关会探测健康接口，只有部署模型与目录模型一致时才向前端标记为可用。
