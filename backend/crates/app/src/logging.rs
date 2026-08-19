use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};

use chrono::{SecondsFormat, Utc};
use serde_json::{Map, Value};
use tracing::{
    Event, Subscriber,
    field::{Field, Visit},
    span::{Attributes, Id, Record},
};
use tracing_subscriber::{
    EnvFilter, Layer,
    layer::{Context, SubscriberExt},
    registry::LookupSpan,
    util::SubscriberInitExt,
};
use uuid::Uuid;

use crate::{AppError, AppResult};

/// 初始化统一日志：终端保留完整 JSON，同时写入服务日志和按 Workflow 分流的任务日志。
///
/// 文件采用 JSON Lines 格式，每行都是一条完整事件。服务日志用于排查启动、配置和
/// Restate SDK 等没有任务上下文的问题；携带合法 `workflow_id` 的事件还会追加到
/// `tasks/<workflow_id>.log`，因此并发 Workflow 也始终拥有各自独立的日志文件。
pub fn init_logging(service: &'static str, log_root: &Path) -> AppResult<()> {
    let filter =
        EnvFilter::try_from_env("TKTKGO_LOG").unwrap_or_else(|_| "info,tktkgo=debug".into());
    let file_layer = JsonFileLayer::new(service, log_root)?;

    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().json().flatten_event(true))
        .with(file_layer)
        .try_init()
        .map_err(|error| AppError::Config(format!("初始化日志订阅器失败: {error}")))?;
    tracing::info!(
        service,
        log_root = %log_root.display(),
        service_log = %log_root.join(format!("{service}.log")).display(),
        task_log_root = %log_root.join("tasks").display(),
        "文件日志初始化完成"
    );
    Ok(())
}

#[derive(Debug)]
struct JsonFileLayer {
    service: &'static str,
    service_log: PathBuf,
    task_log_root: PathBuf,
    service_writer: Mutex<File>,
}

impl JsonFileLayer {
    fn new(service: &'static str, log_root: &Path) -> AppResult<Self> {
        let task_log_root = log_root.join("tasks");
        std::fs::create_dir_all(&task_log_root).map_err(|error| {
            AppError::Config(format!(
                "无法创建日志目录 {}: {error}",
                task_log_root.display()
            ))
        })?;
        let service_log = log_root.join(format!("{service}.log"));
        let service_writer = Mutex::new(open_log_file(&service_log)?);
        Ok(Self {
            service,
            service_log,
            task_log_root,
            service_writer,
        })
    }

    fn write_event(&self, workflow_id: Option<Uuid>, event: Value) {
        let mut line = match serde_json::to_vec(&event) {
            Ok(line) => line,
            Err(error) => {
                eprintln!("[logging] JSON 日志序列化失败: {error}");
                return;
            }
        };
        line.push(b'\n');

        self.write_service_line(&line);
        if let Some(workflow_id) = workflow_id {
            let task_log = self.task_log_root.join(format!("{workflow_id}.log"));
            self.write_task_line(&task_log, &line);
        }
    }

    fn write_service_line(&self, line: &[u8]) {
        let mut writer = match self.service_writer.lock() {
            Ok(writer) => writer,
            Err(error) => {
                eprintln!(
                    "[logging] 服务日志文件锁已损坏（{}）: {error}",
                    self.service_log.display()
                );
                return;
            }
        };
        if let Err(error) = write_and_flush(&mut writer, line) {
            eprintln!(
                "[logging] 写入服务日志文件 {} 失败: {error}",
                self.service_log.display()
            );
        }
    }

    fn write_task_line(&self, path: &Path, line: &[u8]) {
        // 任务文件按事件打开并在写完后关闭，避免常驻 Workflow 服务随着历史任务增加
        // 而无限占用文件描述符。O_APPEND 保证 API 与 Workflow 进程都从文件末尾写入。
        let mut file = match open_log_file(path) {
            Ok(file) => file,
            Err(error) => {
                eprintln!("[logging] {error}");
                return;
            }
        };
        if let Err(error) = write_and_flush(&mut file, line) {
            eprintln!(
                "[logging] 写入任务日志文件 {} 失败: {error}",
                path.display()
            );
        }
    }
}

fn write_and_flush(file: &mut File, line: &[u8]) -> std::io::Result<()> {
    // 每条事件立即刷新，方便任务运行期间直接 tail 文件排障；日志量远小于模型和
    // 渲染 I/O，这里的同步写入不会成为生成流水线的性能瓶颈。
    file.write_all(line)?;
    file.flush()
}

fn open_log_file(path: &Path) -> AppResult<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| AppError::Config(format!("无法打开日志文件 {}: {error}", path.display())))
}

#[derive(Clone, Debug, Default)]
struct RecordedFields(Map<String, Value>);

#[derive(Default)]
struct JsonVisitor(Map<String, Value>);

impl JsonVisitor {
    fn insert(&mut self, field: &Field, value: Value) {
        self.0.insert(field.name().to_owned(), value);
    }
}

impl Visit for JsonVisitor {
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.insert(field, Value::from(value));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.insert(field, Value::from(value));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.insert(field, Value::from(value));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.insert(field, Value::from(value));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.insert(field, Value::from(value));
    }

    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.insert(field, Value::from(value.to_string()));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.insert(field, Value::from(format!("{value:?}")));
    }
}

impl<S> Layer<S> for JsonFileLayer
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut visitor = JsonVisitor::default();
        attrs.record(&mut visitor);
        span.extensions_mut().insert(RecordedFields(visitor.0));
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut visitor = JsonVisitor::default();
        values.record(&mut visitor);
        let mut extensions = span.extensions_mut();
        if let Some(fields) = extensions.get_mut::<RecordedFields>() {
            fields.0.extend(visitor.0);
        } else {
            extensions.insert(RecordedFields(visitor.0));
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let mut visitor = JsonVisitor::default();
        event.record(&mut visitor);
        let event_fields = visitor.0;

        let mut inherited_fields = Map::new();
        let mut spans = Vec::new();
        if let Some(scope) = ctx.event_scope(event) {
            for span in scope.from_root() {
                let mut span_value = Map::new();
                span_value.insert("name".into(), Value::from(span.name()));
                if let Some(fields) = span.extensions().get::<RecordedFields>() {
                    inherited_fields.extend(fields.0.clone());
                    span_value.extend(fields.0.clone());
                }
                spans.push(Value::Object(span_value));
            }
        }

        // 事件字段比 Span 字段更具体，因此同名时由事件覆盖。workflow_id 只接受 UUID，
        // 防止任意日志字段被当成文件名，彻底消除目录穿越和非法路径的可能。
        inherited_fields.extend(event_fields);
        let workflow_id = inherited_fields
            .get("workflow_id")
            .and_then(Value::as_str)
            .and_then(|value| Uuid::parse_str(value).ok());

        let metadata = event.metadata();
        let mut output = Map::new();
        output.insert(
            "timestamp".into(),
            Value::from(Utc::now().to_rfc3339_opts(SecondsFormat::Micros, true)),
        );
        output.insert("level".into(), Value::from(metadata.level().to_string()));
        output.insert("service".into(), Value::from(self.service));
        output.insert("target".into(), Value::from(metadata.target()));
        output.extend(inherited_fields);
        if !spans.is_empty() {
            output.insert("spans".into(), Value::Array(spans));
        }
        self.write_event(workflow_id, Value::Object(output));
    }
}
