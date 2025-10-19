use crate::error::{Error, Result};
use crate::models::{ErrorEvent, UsageEvent};
use reqwest::Client;
use tokio::time::Duration;

pub struct TelemetryModule {
    client: Client,
    business_api_url: String,
}

// 检测模块
impl TelemetryModule {
    pub fn new(business_api_url: String) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(|e| Error::Http(e))?;

        Ok(Self {
            client,
            business_api_url,
        })
    }

    /// 异步上报错误，不等待结果
    pub fn report_error(&self, event: ErrorEvent) {
        let client = self.client.clone();
        let url = format!("{}/v1/telemetry/errors", self.business_api_url);

        // 异步上报，不阻塞主流程
        tokio::spawn(async move {
            let _ = client.post(&url).json(&event).send().await;
            // 忽略上报结果，避免影响主流程
        });
    }

    /// 异步上报使用量，不等待结果
    pub fn report_usage(&self, event: UsageEvent) {
        let client = self.client.clone();
        let url = format!("{}/v1/telemetry/usage", self.business_api_url);

        // 异步上报，不阻塞主流程
        tokio::spawn(async move {
            let _ = client.post(&url).json(&event).send().await;
            // 忽略上报结果，避免影响主流程
        });
    }
}
