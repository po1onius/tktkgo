use reqwest::Client;
use tracing::{info, instrument};

use crate::{
    AppError, AppResult,
    domain::{RenderJobRequest, RenderJobResult},
};

#[derive(Clone)]
pub struct RenderClient {
    client: Client,
    base_url: String,
}

impl RenderClient {
    pub fn new(base_url: String) -> Self {
        Self {
            client: Client::new(),
            base_url,
        }
    }

    #[instrument(skip(self, request), fields(render_id = %request.render_id, project_id = %request.spec.project_id))]
    pub async fn render(&self, request: &RenderJobRequest) -> AppResult<RenderJobResult> {
        let response = self
            .client
            .post(format!("{}/renders", self.base_url))
            .json(request)
            .send()
            .await
            .map_err(|err| AppError::external("remotion", err.to_string()))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|err| AppError::external("remotion", err.to_string()))?;
        if !status.is_success() {
            return Err(AppError::external(
                "remotion",
                format!("HTTP {status}: {body}"),
            ));
        }
        let result: RenderJobResult = serde_json::from_str(&body)?;
        info!(render_id = %request.render_id, public_url = %result.public_url, "Remotion 渲染完成");
        Ok(result)
    }
}
