SHELL := /bin/bash
.DEFAULT_GOAL := help

UV ?= uv
GIT ?= git
FASTER_WHISPER_CONFIG ?= faster-whisper/provider.toml
COSYVOICE_CONFIG ?= cosyvoice/provider.toml
COSYVOICE_PYTHON ?= cosyvoice/.venv/bin/python
COSYVOICE_REQUIREMENTS ?= cosyvoice/requirements-cpu.txt
# CosyVoice 3 尚无对应的稳定 release tag，固定官方主线的已验证提交，避免每次部署漂移。
COSYVOICE_REVISION ?= 074ca6dc9e80a2f424f1f74b48bdd7d3fea531cc

.PHONY: help faster-whisper cosyvoice prepare-cosyvoice

help:
	@echo "独立本地模型部署目标："
	@echo "  make faster-whisper  自动准备权重并启动字幕服务"
	@echo "  make cosyvoice       自动准备官方代码、环境、权重并启动口播服务"

faster-whisper: ## 独立启动 faster-whisper，权重缺失时自动下载
	@command -v "$(UV)" >/dev/null || { echo "[faster-whisper] 未找到 uv，请手动安装后重试" >&2; exit 1; }
	@if [[ ! -f "$(FASTER_WHISPER_CONFIG)" ]]; then \
		cp faster-whisper/provider.example.toml "$(FASTER_WHISPER_CONFIG)"; \
		echo "[faster-whisper] 已创建独立配置 $(FASTER_WHISPER_CONFIG)"; \
	fi
	$(UV) sync --project faster-whisper --frozen
	@echo "[faster-whisper] 启动时会自动下载并加载配置模型"
	TKTKGO_FASTER_WHISPER_CONFIG="$(abspath $(FASTER_WHISPER_CONFIG))" \
		$(UV) run --project faster-whisper --frozen python faster-whisper/provider.py

prepare-cosyvoice: ## 准备 CosyVoice 官方仓库和独立 Python 3.10 环境
	@command -v "$(UV)" >/dev/null || { echo "[cosyvoice] 未找到 uv，请手动安装后重试" >&2; exit 1; }
	@command -v "$(GIT)" >/dev/null || { echo "[cosyvoice] 未找到 git，请手动安装后重试" >&2; exit 1; }
	@if [[ ! -d cosyvoice/CosyVoice/.git ]]; then \
		echo "[cosyvoice] 下载 CosyVoice 官方仓库及子模块"; \
		$(GIT) clone https://github.com/FunAudioLLM/CosyVoice.git cosyvoice/CosyVoice; \
		$(GIT) -C cosyvoice/CosyVoice checkout "$(COSYVOICE_REVISION)"; \
		$(GIT) -C cosyvoice/CosyVoice submodule update --init --recursive; \
	fi
	@actual_revision="$$(git -C cosyvoice/CosyVoice rev-parse HEAD)"; \
	if [[ "$$actual_revision" != "$(COSYVOICE_REVISION)" ]]; then \
		echo "[cosyvoice] 官方仓库版本不匹配，当前 $$actual_revision，要求 $(COSYVOICE_REVISION)" >&2; \
		echo "[cosyvoice] 请手动处理本地仓库改动并切换到要求版本" >&2; \
		exit 1; \
	fi
	@if [[ ! -x "$(COSYVOICE_PYTHON)" ]]; then \
		echo "[cosyvoice] 创建官方兼容的 Python 3.10 环境"; \
		$(UV) venv --python 3.10 cosyvoice/.venv; \
	fi
	@echo "[cosyvoice] 同步官方依赖和本地 HTTP 服务依赖"
	# openai-whisper 旧版构建脚本隐式依赖 pkg_resources；setuptools 81+ 已逐步
	# 移除该接口，因此仅为这个上游固定版本准备最后一个兼容稳定版。
	$(UV) pip install --python "$(COSYVOICE_PYTHON)" setuptools==80.9.0
	# 默认安装无 NVIDIA 驱动也能工作的 CPU 推理依赖；如已按官方方式准备 GPU
	# 环境，可通过 COSYVOICE_REQUIREMENTS 指向自己的受控依赖文件。
	$(UV) pip install --python "$(COSYVOICE_PYTHON)" \
		--index-strategy unsafe-best-match \
		--no-build-isolation-package openai-whisper \
		-r "$(COSYVOICE_REQUIREMENTS)"
	$(UV) pip install --python "$(COSYVOICE_PYTHON)" \
		fastapi==0.141.1 uvicorn==0.52.3 tomli==2.4.1

cosyvoice: prepare-cosyvoice ## 独立启动 CosyVoice，权重缺失时自动下载
	@if [[ ! -f "$(COSYVOICE_CONFIG)" ]]; then \
		cp cosyvoice/provider.example.toml "$(COSYVOICE_CONFIG)"; \
		echo "[cosyvoice] 已创建独立配置 $(COSYVOICE_CONFIG)"; \
	fi
	@if [[ ! -f cosyvoice/voices.json ]]; then \
		cp cosyvoice/voices.example.json cosyvoice/voices.json; \
		echo "[cosyvoice] 已创建 cosyvoice/voices.json，请准备参考 WAV 并修正音色配置后重新启动" >&2; \
		exit 1; \
	fi
	@echo "[cosyvoice] 启动时会自动下载并加载配置模型"
	TKTKGO_COSYVOICE_CONFIG="$(abspath $(COSYVOICE_CONFIG))" \
		"$(COSYVOICE_PYTHON)" cosyvoice/provider.py
