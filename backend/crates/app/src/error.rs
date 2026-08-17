use thiserror::Error;

pub type AppResult<T> = Result<T, AppError>;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("配置错误: {0}")]
    Config(String),
    #[error("参数错误: {0}")]
    Validation(String),
    #[error("资源不存在: {0}")]
    NotFound(String),
    #[error("当前状态不允许执行该操作: {0}")]
    Conflict(String),
    #[error("数据库操作失败: {0}")]
    Database(#[from] diesel::result::Error),
    #[error("数据库连接池操作失败: {0}")]
    Pool(String),
    #[error("外部服务 {service} 调用失败: {message}")]
    External {
        service: &'static str,
        message: String,
    },
    #[error("序列化失败: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("文件操作失败: {0}")]
    Io(#[from] std::io::Error),
    #[error("内部错误: {0}")]
    Internal(String),
}

impl AppError {
    pub fn external(service: &'static str, message: impl Into<String>) -> Self {
        Self::External {
            service,
            message: message.into(),
        }
    }
}
