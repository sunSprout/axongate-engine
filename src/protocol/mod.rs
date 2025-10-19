pub mod adapter;
pub mod anthropic;
pub mod detector;
pub mod openai;

use crate::error::Result;
use crate::models::{ClientProtocol, TargetProtocol};
use async_trait::async_trait;
use bytes::Bytes;
use futures::Stream;
use std::pin::Pin;

#[async_trait]
pub trait ProtocolAdapter: Send + Sync {
    async fn transform_request(
        &self,
        source_protocol: &ClientProtocol,
        target_protocol: &TargetProtocol,
        target_model: &str,
        request_body: Bytes,
    ) -> Result<Bytes>;

    async fn transform_response(
        &self,
        source_protocol: &TargetProtocol,
        target_protocol: &ClientProtocol,
        response_body: Bytes,
    ) -> Result<Bytes>;

    async fn transform_stream_chunk(
        &self,
        source_protocol: &TargetProtocol,
        target_protocol: &ClientProtocol,
        stream: impl Stream<Item = Result<Bytes>> + Send + 'static,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<Bytes>> + Send>>>;
}
