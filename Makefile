SHELL := /bin/bash
.DEFAULT_GOAL := dev

PODMAN ?= podman
COMPOSE_FILE ?= compose.yaml
CARGO ?= cargo
PNPM ?= pnpm
CURL ?= curl
ENV_FILE ?= .env

API_URL ?= http://127.0.0.1:8000
WORKFLOW_URL ?= http://127.0.0.1:9080
RESTATE_ADMIN_URL ?= http://127.0.0.1:9070
# Restate 位于容器内，通过 Compose 配置的宿主机网关访问本地 Workflow。
WORKFLOW_DEPLOYMENT_URI ?= http://host.docker.internal:9080

.PHONY: dev

dev: ## 启动基础设施，并在宿主机构建、运行全部应用服务
	@set -Eeuo pipefail; \
	for tool in "$(PODMAN)" "$(CARGO)" node "$(PNPM)" "$(CURL)" ffmpeg ffprobe; do \
		if ! command -v "$$tool" >/dev/null; then \
			echo "[tools] 未找到 $$tool，请手动安装后重试" >&2; \
			exit 1; \
		fi; \
	done; \
	if ! $(PODMAN) compose version >/dev/null 2>&1; then \
		echo "[tools] podman compose 不可用，请手动安装 Compose provider" >&2; \
		exit 1; \
	fi; \
	if [[ ! -f "$(ENV_FILE)" ]]; then \
		cp .env.example "$(ENV_FILE)"; \
		echo "[env] 已创建 $(ENV_FILE)，请填写 TKTKGO_OPENAI_API_KEY 后重新执行 make" >&2; \
		exit 1; \
	fi; \
	set -a; source "$(ENV_FILE)"; set +a; \
	if [[ -z "$${TKTKGO_OPENAI_API_KEY:-}" ]]; then \
		echo "[env] TKTKGO_OPENAI_API_KEY 不能为空，请编辑 $(ENV_FILE)" >&2; \
		exit 1; \
	fi; \
	api_pid=""; \
	workflow_pid=""; \
	render_pid=""; \
	web_pid=""; \
	infra_started=0; \
	cleanup() { \
		status=$$?; \
		trap - EXIT INT TERM; \
		echo "[local] 正在停止本地应用服务"; \
		for pid in "$$api_pid" "$$workflow_pid" "$$render_pid" "$$web_pid"; do \
			if [[ -n "$$pid" ]] && kill -0 "$$pid" 2>/dev/null; then \
				kill "$$pid" 2>/dev/null || true; \
			fi; \
		done; \
		for pid in "$$api_pid" "$$workflow_pid" "$$render_pid" "$$web_pid"; do \
			if [[ -n "$$pid" ]]; then wait "$$pid" 2>/dev/null || true; fi; \
		done; \
		if [[ "$$infra_started" == "1" ]]; then \
			echo "[infra] 正在停止 PostgreSQL 和 Restate，数据卷会保留"; \
			$(PODMAN) compose -f "$(COMPOSE_FILE)" down || true; \
		fi; \
		exit "$$status"; \
	}; \
	trap cleanup EXIT INT TERM; \
	echo "[infra] 使用 Podman Compose 启动 PostgreSQL 和 Restate"; \
	$(PODMAN) compose -f "$(COMPOSE_FILE)" up -d postgres restate; \
	infra_started=1; \
	$(PODMAN) compose -f "$(COMPOSE_FILE)" ps; \
	echo "[build] 安装 Node.js 依赖"; \
	$(PNPM) install --frozen-lockfile; \
	echo "[build] 构建 Rust workspace"; \
	$(CARGO) build --workspace; \
	echo "[build] 构建 Web 和 Render"; \
	$(PNPM) build; \
	echo "[api] 先启动 API，以顺序完成数据库迁移"; \
	$(CARGO) run -p tktkgo-api & \
	api_pid=$$!; \
	api_ready=0; \
	for attempt in $$(seq 1 90); do \
		if ! kill -0 "$$api_pid" 2>/dev/null; then \
			echo "[api] 服务提前退出，请检查上方日志" >&2; \
			exit 1; \
		fi; \
		if $(CURL) --silent --fail --output /dev/null --connect-timeout 1 "$(API_URL)/health"; then \
			api_ready=1; \
			break; \
		fi; \
		sleep 1; \
	done; \
	if [[ "$$api_ready" != "1" ]]; then \
		echo "[api] 90 秒内未就绪，请检查 PostgreSQL 和 API 日志" >&2; \
		exit 1; \
	fi; \
	echo "[local] 启动 Workflow、Render 和 Web"; \
	$(CARGO) run -p tktkgo-workflow & workflow_pid=$$!; \
	$(PNPM) --filter @tktkgo/render dev & render_pid=$$!; \
	$(PNPM) --filter @tktkgo/web dev & web_pid=$$!; \
	workflow_ready=0; \
	for attempt in $$(seq 1 90); do \
		if ! kill -0 "$$workflow_pid" 2>/dev/null; then \
			echo "[workflow] 服务提前退出，请检查上方日志" >&2; \
			exit 1; \
		fi; \
		# Restate SDK 端点只接受明文 HTTP/2（h2c），普通 HTTP/1.1 探测会触发 PROTOCOL_ERROR。 \
		if $(CURL) --http2-prior-knowledge --silent --fail --output /dev/null \
			--connect-timeout 1 "$(WORKFLOW_URL)/health"; then \
			workflow_ready=1; \
			break; \
		fi; \
		sleep 1; \
	done; \
	if [[ "$$workflow_ready" != "1" ]]; then \
		echo "[workflow] 90 秒内未就绪，请检查 Workflow 日志" >&2; \
		exit 1; \
	fi; \
	echo "[restate] 注册 Workflow 端点：$(WORKFLOW_DEPLOYMENT_URI)"; \
	registered=0; \
	for attempt in $$(seq 1 30); do \
		if $(CURL) --silent --show-error --fail-with-body \
			-X POST "$(RESTATE_ADMIN_URL)/deployments" \
			-H 'content-type: application/json' \
			-d '{"uri":"$(WORKFLOW_DEPLOYMENT_URI)"}'; then \
			registered=1; \
			echo; \
			break; \
		fi; \
		echo "[restate] 管理接口尚未就绪，正在进行第 $$attempt/30 次重试" >&2; \
		sleep 1; \
	done; \
	if [[ "$$registered" != "1" ]]; then \
		echo "[restate] Workflow 端点注册失败" >&2; \
		exit 1; \
	fi; \
	echo "[local] 全部服务已启动，访问 http://127.0.0.1:3000；按 Ctrl+C 停止"; \
	set +e; \
	wait -n "$$api_pid" "$$workflow_pid" "$$render_pid" "$$web_pid"; \
	status=$$?; \
	set -e; \
	echo "[local] 检测到服务退出，退出码：$$status" >&2; \
	exit "$$status"
