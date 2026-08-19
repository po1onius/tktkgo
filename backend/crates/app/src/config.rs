use std::{env, net::SocketAddr, path::PathBuf};

use crate::{AppError, AppResult};

#[derive(Clone, Debug)]
pub struct Settings {
    pub database_url: String,
    pub api_addr: SocketAddr,
    pub workflow_addr: SocketAddr,
    pub restate_ingress_url: String,
    pub renderer_url: String,
    pub public_asset_base_url: String,
    pub asset_root: PathBuf,
    pub web_root: PathBuf,
    pub log_root: PathBuf,
    pub model_gateway_url: String,
}

impl Settings {
    /// 从环境变量加载配置。环境不正确时直接报错，不在业务代码中添加猜测性兼容逻辑。
    pub fn from_env() -> AppResult<Self> {
        dotenvy::dotenv().ok();
        Ok(Self {
            database_url: required("TKTKGO_DATABASE_URL")?,
            api_addr: parse_addr("TKTKGO_API_ADDR", "0.0.0.0:8000")?,
            workflow_addr: parse_addr("TKTKGO_WORKFLOW_ADDR", "0.0.0.0:9080")?,
            restate_ingress_url: trim_url(value(
                "TKTKGO_RESTATE_INGRESS_URL",
                "http://localhost:8080",
            )),
            renderer_url: trim_url(value("TKTKGO_RENDERER_URL", "http://localhost:8090")),
            public_asset_base_url: trim_url(value(
                "TKTKGO_PUBLIC_ASSET_BASE_URL",
                "http://localhost:8000/assets",
            )),
            asset_root: PathBuf::from(value("TKTKGO_ASSET_ROOT", "./storage")),
            web_root: PathBuf::from(value("TKTKGO_WEB_ROOT", "./web/out")),
            log_root: PathBuf::from(value("TKTKGO_LOG_ROOT", "./logs")),
            model_gateway_url: trim_url(value("TKTKGO_MODEL_GATEWAY_URL", "http://127.0.0.1:8110")),
        })
    }
}

fn required(name: &str) -> AppResult<String> {
    env::var(name)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| AppError::Config(format!("缺少必填环境变量 {name}")))
}

fn value(name: &str, default: &str) -> String {
    env::var(name).unwrap_or_else(|_| default.to_owned())
}

fn parse_addr(name: &str, default: &str) -> AppResult<SocketAddr> {
    value(name, default)
        .parse()
        .map_err(|err| AppError::Config(format!("{name} 不是合法地址: {err}")))
}

fn trim_url(value: String) -> String {
    value.trim_end_matches('/').to_owned()
}
