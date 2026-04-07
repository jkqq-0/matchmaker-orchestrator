use matchmaker_orchestrator::AppState;
use matchmaker_orchestrator::service::ProjectService;
use matchmaker_orchestrator::storage::MockStorageProvider;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::Semaphore;
use uuid::Uuid;

async fn setup_app_state() -> AppState {
    dotenvy::dotenv().ok();
    let pool =
        sqlx::PgPool::connect(&std::env::var("DATABASE_URL").expect("DATABASE_URL must be set"))
            .await
            .unwrap();
    AppState {
        pool,
        storage: Arc::new(MockStorageProvider::new()),
        http_client: reqwest::Client::new(),
        openai_api_key: "test".to_string(),
        openai_endpoint: "test".to_string(),
        resume_schema: json!({}),
        ner_engine: std::sync::Arc::new(std::sync::Mutex::new(
            matchmaker_orchestrator::pii_scrubber::NerEngine::new().unwrap(),
        )),
        semaphore: Arc::new(Semaphore::new(1)),
        jwt_secret: "test".to_string(),
    }
}

#[tokio::test]
async fn test_job_error_persistence() {
    let state = setup_app_state().await;
    let service = ProjectService::new(state.clone());

    let job_id = Uuid::new_v4();
    let project_id = Uuid::new_v4();

    // 1. Create a job record
    sqlx::query!(
        "INSERT INTO jobs (id, status) VALUES ($1, 'processing')",
        job_id
    )
    .execute(&state.pool)
    .await
    .unwrap();

    // 2. Record an error
    service
        .record_job_error(job_id, project_id, "Test Error Message".to_string())
        .await;

    // 3. Verify JSONB content
    let job = sqlx::query!("SELECT rust_error FROM jobs WHERE id = $1", job_id)
        .fetch_one(&state.pool)
        .await
        .unwrap();

    let rust_error = job.rust_error.unwrap();
    let projects = rust_error.get("projects").unwrap().as_array().unwrap();

    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0]["error"], "Test Error Message");
    assert_eq!(projects[0]["id"], project_id.to_string());

    // 4. Record a second error to check appending (|| operator)
    let project_id2 = Uuid::new_v4();
    service
        .record_job_error(job_id, project_id2, "Second Error".to_string())
        .await;

    let job = sqlx::query!("SELECT rust_error FROM jobs WHERE id = $1", job_id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let projects = job
        .rust_error
        .unwrap()
        .get("projects")
        .unwrap()
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(projects.len(), 2);
    assert_eq!(projects[1]["error"], "Second Error");

    // Cleanup
    sqlx::query!("DELETE FROM jobs WHERE id = $1", job_id)
        .execute(&state.pool)
        .await
        .unwrap();
}

use matchmaker_orchestrator::service::JobStatus;

#[tokio::test]
async fn test_job_readiness_logic() {
    let state = setup_app_state().await;
    let service = ProjectService::new(state.clone());

    let job_id = Uuid::new_v4();
    let term = format!("TestTerm-{}", Uuid::new_v4()); // Unique term

    // 1. Create a job record
    sqlx::query!(
        "INSERT INTO jobs (id, status, term) VALUES ($1, 'pending', $2)",
        job_id,
        term
    )
    .execute(&state.pool)
    .await
    .unwrap();

    // 2. Add a project upload that is still 'processing'
    let upload_id = Uuid::new_v4();
    sqlx::query!("INSERT INTO project_uploads (id, filename, status, job_id, term) VALUES ($1, 'p.csv', 'processing', $2, $3)", upload_id, job_id, term)
        .execute(&state.pool).await.unwrap();

    // 3. Trigger check - should NOT become ready (still processing)
    service.maybe_mark_job_as_ready(job_id).await;
    let job = sqlx::query_as::<_, (JobStatus,)>("SELECT status FROM jobs WHERE id = $1")
        .bind(job_id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(job.0, JobStatus::Pending);

    // 4. Mark job as processing (as the real service does) and upload as 'completed'
    sqlx::query!(
        "UPDATE jobs SET status = 'processing' WHERE id = $1",
        job_id
    )
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query!(
        "UPDATE project_uploads SET status = 'completed' WHERE id = $1",
        upload_id
    )
    .execute(&state.pool)
    .await
    .unwrap();

    // 5. Trigger check - should become ready (all uploads for this job_id are now completed)
    service.maybe_mark_job_as_ready(job_id).await;
    let job = sqlx::query_as::<_, (JobStatus,)>("SELECT status FROM jobs WHERE id = $1")
        .bind(job_id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(job.0, JobStatus::Ready);

    // Cleanup
    sqlx::query!("DELETE FROM project_uploads WHERE id = $1", upload_id)
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query!("DELETE FROM jobs WHERE id = $1", job_id)
        .execute(&state.pool)
        .await
        .unwrap();
}
