pub mod auth;
pub mod config;
pub mod requests;
pub mod service;
pub mod storage;

use crate::storage::StorageProvider;
use serde_json::Value;
use sqlx::PgPool;
use std::sync::Arc;
use tokio::sync::Semaphore;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub storage: Arc<dyn StorageProvider>,
    pub http_client: reqwest::Client,
    pub openai_api_key: String,
    pub openai_endpoint: String,
    pub resume_schema: Value,
    pub semaphore: Arc<Semaphore>,
    pub jwt_secret: String,
}
