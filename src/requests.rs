pub mod openai;

use crate::AppState;
use crate::service::{JobStatus, ProjectService, ResumeService};
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;
use serde_json::json;
use tokio::task;
use uuid::Uuid;

#[derive(Deserialize, Debug)]
pub struct WebhookPayload {
    pub record: FileTrackingTableRecord,
}

#[derive(Deserialize, Debug)]
pub struct FileTrackingTableRecord {
    id: Uuid,
    filename: String,
    job_id: Option<Uuid>,
}

pub async fn handle_single_upload(
    State(state): State<AppState>,
    Json(payload): Json<WebhookPayload>,
) -> impl IntoResponse {
    let filename = payload.record.filename.clone();
    let id = payload.record.id;
    let job_id = payload.record.job_id;
    tracing::info!("scrape handler accessed");

    // Mark job as Processing immediately, before the background task acquires the semaphore.
    if let Some(job_id) = job_id {
        let _ = sqlx::query!(
            "UPDATE jobs SET status = $1 WHERE id = $2 AND status = $3",
            JobStatus::Processing as JobStatus,
            job_id,
            JobStatus::Pending as JobStatus
        )
        .execute(&state.pool)
        .await;
    }

    let service = ResumeService::new(state);

    task::spawn(async move {
        service.process_resume_upload(id, filename).await;
    });

    (
        StatusCode::ACCEPTED,
        Json(json!({"status": "processing", "message": "We're working on it!"})),
    )
}

pub async fn handle_batch_upload(
    State(state): State<AppState>,
    Json(payload): Json<WebhookPayload>,
) -> impl IntoResponse {
    let filename = payload.record.filename.clone();
    let id = payload.record.id;
    let job_id = payload.record.job_id;
    tracing::info!("batch upload handler accessed");

    // Mark job as Processing immediately, before the background task acquires the semaphore.
    if let Some(job_id) = job_id {
        let _ = sqlx::query!(
            "UPDATE jobs SET status = $1 WHERE id = $2 AND status = $3",
            JobStatus::Processing as JobStatus,
            job_id,
            JobStatus::Pending as JobStatus
        )
        .execute(&state.pool)
        .await;
    }

    let service = ResumeService::new(state);

    task::spawn(async move {
        service.handle_batch_extraction(id, filename).await;
    });

    (
        StatusCode::ACCEPTED,
        Json(json!({"status": "processing", "message": "We're working on it!"})),
    )
}

pub async fn handle_project_upload(
    State(state): State<AppState>,
    Json(payload): Json<WebhookPayload>,
) -> impl IntoResponse {
    let filename = payload.record.filename.clone();
    let id = payload.record.id;
    let job_id = payload.record.job_id;
    tracing::info!("project upload handler accessed");

    // Mark job as Processing immediately, before the background task acquires the semaphore.
    if let Some(job_id) = job_id {
        let _ = sqlx::query!(
            "UPDATE jobs SET status = $1 WHERE id = $2 AND status = $3",
            JobStatus::Processing as JobStatus,
            job_id,
            JobStatus::Pending as JobStatus
        )
        .execute(&state.pool)
        .await;
    }

    let service = ProjectService::new(state);

    task::spawn(async move {
        service.process_project_spreadsheet(id, filename).await;
    });

    (
        StatusCode::ACCEPTED,
        Json(json!({"status": "processing", "message": "Processing projects..."})),
    )
}
