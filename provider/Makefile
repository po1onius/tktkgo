SHELL := /bin/bash
.DEFAULT_GOAL := start

UV ?= uv
CONFIG_FILE ?= providers.toml

.PHONY: start sync

sync: ## 同步 Provider 网关的锁定依赖
	@command -v "$(UV)" >/dev/null || { echo "[provider] 未找到 uv，请手动安装后重试" >&2; exit 1; }
	$(UV) sync --frozen

start: sync ## 仅启动固定 Provider 网关，不拉起任何本地模型
	@if [[ ! -f "$(CONFIG_FILE)" ]]; then \
		cp providers.example.toml "$(CONFIG_FILE)"; \
		echo "[provider] 已创建 $(CONFIG_FILE)，请填写所选远程 Provider 的 Key 后重新启动"; \
		exit 1; \
	fi
	@echo "[provider] 使用独立配置 $(abspath $(CONFIG_FILE)) 启动固定网关"
	TKTKGO_PROVIDERS_CONFIG="$(abspath $(CONFIG_FILE))" \
		$(UV) run --frozen python provider.py
