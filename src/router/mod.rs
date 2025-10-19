use crate::cache::Cache;
use crate::config::BusinessApiConfig;
use crate::error::{Error, Result};
use crate::models::{RouteConfig, RouteRequest, RouteResponse};
use reqwest::Client;
use std::sync::Arc;
use tracing::{error, info};

pub struct Router {
    cache: Arc<Cache>,
    client: Client,
    business_api_config: BusinessApiConfig,
}

impl Router {
    pub fn new(cache: Arc<Cache>, business_api_config: BusinessApiConfig) -> Result<Self> {
        let client = Client::builder()
            .timeout(business_api_config.timeout)
            .build()
            .map_err(|e| Error::Http(e))?;

        Ok(Self {
            cache,
            client,
            business_api_config,
        })
    }

    pub async fn resolve_route(
        &self,
        user_token: &str,
        requested_model: &str,
    ) -> Result<Vec<RouteConfig>> {
        // 1. 先查缓存
        if let Some(configs) = self.cache.get(user_token, requested_model).await {
            if !configs.is_empty() {
                return Ok(configs);
            }
        }

        // 2. 缓存未命中，调用业务 API
        let configs = self
            .fetch_from_business_api(user_token, requested_model)
            .await?;

        // 3. 更新缓存
        if !configs.is_empty() {
            self.cache
                .set(user_token, requested_model, configs.clone())
                .await;
        }

        Ok(configs.clone())
    }

    async fn fetch_from_business_api(
        &self,
        user_token: &str,
        requested_model: &str,
    ) -> Result<Vec<RouteConfig>> {
        let url = format!("{}/v1/route/resolve", self.business_api_config.base_url);

        let request = RouteRequest {
            token: user_token.to_string(),
            model: requested_model.to_string(),
        };

        let mut retry_count = 0;
        let max_retries = self.business_api_config.retry_attempts;

        loop {
            let response = self.client.post(&url).json(&request).send().await;

            match response {
                Ok(resp) => {
                    if resp.status().is_success() {
                        let route_response: RouteResponse =
                            resp.json().await.map_err(|e| Error::Http(e))?;

                        if route_response.success {
                            return Ok(route_response.data);
                        } else {
                            return Err(Error::Routing(format!(
                                "Business API returned error: {}",
                                route_response.message
                            )));
                        }
                    } else if resp.status().is_server_error() && retry_count < max_retries {
                        retry_count += 1;
                        tokio::time::sleep(tokio::time::Duration::from_millis(
                            100 * retry_count as u64,
                        ))
                        .await;
                        continue;
                    } else {
                        return Err(Error::Routing(format!(
                            "Business API returned status: {}",
                            resp.status()
                        )));
                    }
                }
                Err(e) if retry_count < max_retries => {
                    retry_count += 1;
                    error!(
                        "Error calling business API: {}. Retrying {}/{}",
                        e, retry_count, max_retries
                    );
                    tokio::time::sleep(tokio::time::Duration::from_millis(
                        100 * retry_count as u64,
                    ))
                    .await;
                    continue;
                }
                Err(e) => {
                    return Err(Error::Http(e));
                }
            }
        }
    }

    pub async fn remove_failed_route(
        &self,
        user_token: &str,
        requested_model: &str,
        failed_config: &RouteConfig,
    ) {
        self.cache
            .remove_config(user_token, requested_model, failed_config)
            .await;
    }
}
