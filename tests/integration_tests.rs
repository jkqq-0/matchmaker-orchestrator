use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
    routing::post,
};
use jsonwebtoken::{EncodingKey, Header, encode};
use matchmaker_orchestrator::auth::Claims;
use matchmaker_orchestrator::requests::{
    handle_batch_upload, handle_project_upload, handle_single_upload,
};
use matchmaker_orchestrator::service::DocumentStatus;
use matchmaker_orchestrator::storage::{MockStorageProvider, StorageProvider};
use matchmaker_orchestrator::{AppState, auth};
use serde_json::json;
use std::io::Write;
use std::sync::Arc;
use tokio::sync::Semaphore;
use tower::ServiceExt;
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct TestEnv {
    app: Router,
    pool: sqlx::PgPool,
    storage: Arc<MockStorageProvider>,
    jwt_secret: String,
}

async fn setup_test_env() -> TestEnv {
    dotenvy::dotenv().ok();
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .try_init();

    let pool =
        sqlx::PgPool::connect(&std::env::var("DATABASE_URL").expect("DATABASE_URL must be set"))
            .await
            .unwrap();
    let storage = Arc::new(MockStorageProvider::new());
    let jwt_secret = "test-secret".to_string();

    let app_state = AppState {
        pool: pool.clone(),
        storage: storage.clone(),
        http_client: reqwest::Client::new(),
        openai_api_key: "test-key".to_string(),
        openai_endpoint: "http://localhost:1234".to_string(), // Default, tests can override
        resume_schema: json!({}),
        semaphore: Arc::new(Semaphore::new(10)),
        jwt_secret: jwt_secret.clone(),
    };

    let app = Router::new()
        .route("/ingest/projects", post(handle_project_upload))
        .route("/ingest/interns/individual", post(handle_single_upload))
        .route("/ingest/interns/batch", post(handle_batch_upload))
        .route_layer(axum::middleware::from_fn_with_state(
            app_state.clone(),
            auth::auth,
        ))
        .with_state(app_state);

    TestEnv {
        app,
        pool,
        storage,
        jwt_secret,
    }
}

fn create_jwt(secret: &str) -> String {
    let claims = Claims {
        sub: "test-user".to_string(),
        exp: 10000000000, // far in future
        aud: None,
        role: Some("authenticated".to_string()),
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .unwrap()
}

#[tokio::test]
async fn test_project_upload_flow() {
    let env = setup_test_env().await;

    // Mock spreadsheet data
    let csv_data = b"title,description,requirements,manager,deadline,priority,intern_cap\nProject Integration Test,Integration Desc,Req,Mgr,2026-12-31,1,2";
    env.storage
        .put_object("project-spreadsheets", "test.csv", csv_data.to_vec(), None)
        .await
        .unwrap();

    // Create an upload record manually for the test
    let upload_id = Uuid::new_v4();
    sqlx::query!(
        "INSERT INTO project_uploads (id, filename, status) VALUES ($1, $2, 'pending')",
        upload_id,
        "test.csv"
    )
    .execute(&env.pool)
    .await
    .unwrap();

    let token = create_jwt(&env.jwt_secret);
    let payload = json!({
        "record": {
            "id": upload_id,
            "filename": "test.csv"
        }
    });

    let response = env
        .app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/ingest/projects")
                .header("Authorization", format!("Bearer {}", token))
                .header("Content-Type", "application/json")
                .body(Body::from(serde_json::to_vec(&payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::ACCEPTED);

    // Poll database for completion
    let mut success = false;
    for _ in 0..10 {
        let record = sqlx::query_as::<_, (DocumentStatus,)>(
            "SELECT status FROM project_uploads WHERE id = $1",
        )
        .bind(upload_id)
        .fetch_one(&env.pool)
        .await
        .unwrap();

        if matches!(record.0, DocumentStatus::Completed) {
            success = true;
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    }

    assert!(success, "Project upload did not complete in time");

    // Verify projects were inserted
    let projects = sqlx::query!("SELECT title FROM projects WHERE upload_id = $1", upload_id)
        .fetch_all(&env.pool)
        .await
        .unwrap();

    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].title, "Project Integration Test");

    // Cleanup
    sqlx::query!("DELETE FROM projects WHERE upload_id = $1", upload_id)
        .execute(&env.pool)
        .await
        .unwrap();
    sqlx::query!("DELETE FROM project_uploads WHERE id = $1", upload_id)
        .execute(&env.pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn test_resume_upload_flow() {
    let mut env = setup_test_env().await;

    // 1. Setup Mock OpenAI
    let mock_server = MockServer::start().await;
    let mock_response = json!({
        "choices": [
            {
                "message": {
                    "role": "assistant",
                    "content": "{\"name\": \"Alex Rivera\", \"skills\": [\"Rust\", \"Testing\"]}"
                }
            }
        ]
    });

    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_response))
        .mount(&mock_server)
        .await;

    // Update app state with mock server URI
    // We need to recreate the app with the new endpoint
    let app_state = AppState {
        pool: env.pool.clone(),
        storage: env.storage.clone(),
        http_client: reqwest::Client::new(),
        openai_api_key: "test-key".to_string(),
        openai_endpoint: mock_server.uri(),
        resume_schema: json!({}),
        semaphore: Arc::new(Semaphore::new(10)),
        jwt_secret: env.jwt_secret.clone(),
    };

    env.app = Router::new()
        .route("/ingest/interns/individual", post(handle_single_upload))
        .route_layer(axum::middleware::from_fn_with_state(
            app_state.clone(),
            auth::auth,
        ))
        .with_state(app_state);

    // 2. Mock PDF data
    let pdf_path = "archive.zip-resumes/Alex_Rivera_CV.pdf";
    let pdf_bytes = std::fs::read(pdf_path).expect("Failed to read test PDF");
    env.storage
        .put_object("resumes", "Alex_Rivera_CV.pdf", pdf_bytes, None)
        .await
        .unwrap();

    // 3. Create upload record
    let upload_id = Uuid::new_v4();
    sqlx::query!(
        "INSERT INTO resume_uploads (id, filename, status) VALUES ($1, $2, 'pending')",
        upload_id,
        "Alex_Rivera_CV.pdf"
    )
    .execute(&env.pool)
    .await
    .unwrap();

    // 4. Generate JWT & Trigger
    let token = create_jwt(&env.jwt_secret);
    let payload = json!({
        "record": {
            "id": upload_id,
            "filename": "Alex_Rivera_CV.pdf"
        }
    });

    let response = env
        .app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/ingest/interns/individual")
                .header("Authorization", format!("Bearer {}", token))
                .header("Content-Type", "application/json")
                .body(Body::from(serde_json::to_vec(&payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::ACCEPTED);

    // 5. Poll for completion
    let mut success = false;
    for _ in 0..20 {
        let record = sqlx::query_as::<_, (DocumentStatus,)>(
            "SELECT status FROM resume_uploads WHERE id = $1",
        )
        .bind(upload_id)
        .fetch_one(&env.pool)
        .await
        .unwrap();

        if matches!(record.0, DocumentStatus::Completed) {
            success = true;
            break;
        }
        if matches!(record.0, DocumentStatus::Failed) {
            let err = sqlx::query!(
                "SELECT error_message FROM resume_uploads WHERE id = $1",
                upload_id
            )
            .fetch_one(&env.pool)
            .await
            .unwrap();
            panic!("Resume upload failed: {:?}", err.error_message);
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    }

    assert!(success, "Resume upload did not complete in time");

    // 6. Verify DB record
    let resume = sqlx::query!(
        "SELECT filename, structured FROM resumes WHERE upload_id = $1",
        upload_id
    )
    .fetch_one(&env.pool)
    .await
    .unwrap();

    assert_eq!(resume.filename, "Alex_Rivera_CV.pdf");
    assert_eq!(resume.structured.unwrap()["name"], "Alex Rivera");

    // Cleanup
    sqlx::query!("DELETE FROM resumes WHERE upload_id = $1", upload_id)
        .execute(&env.pool)
        .await
        .unwrap();
    sqlx::query!("DELETE FROM resume_uploads WHERE id = $1", upload_id)
        .execute(&env.pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn test_auth_security_gauntlet() {
    let env = setup_test_env().await;

    // 1. Missing Authorization Header
    let res = env.app.clone()
        .oneshot(Request::builder().method("POST").uri("/ingest/projects").body(Body::empty()).unwrap())
        .await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // 2. Malformed Header (not Bearer)
    let res = env.app.clone()
        .oneshot(Request::builder().method("POST").uri("/ingest/projects").header("Authorization", "Basic 123").body(Body::empty()).unwrap())
        .await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // 3. Invalid Secret
    let claims = Claims { sub: "u".to_string(), exp: 10000000000, aud: None, role: Some("authenticated".to_string()) };
    let bad_token = encode(&Header::default(), &claims, &EncodingKey::from_secret(b"wrong-secret")).unwrap();
    let res = env.app.clone()
        .oneshot(Request::builder().method("POST").uri("/ingest/projects").header("Authorization", format!("Bearer {}", bad_token)).body(Body::empty()).unwrap())
        .await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_zip_filtering_chaos() {
    let env = setup_test_env().await;
    
    // Create ZIP with mixed content: one valid PDF, one TXT, and one in a folder
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        
        zip.start_file("valid.pdf", options).unwrap();
        zip.write_all(b"%PDF-1.4").unwrap();
        
        zip.start_file("ignore_me.txt", options).unwrap();
        zip.write_all(b"I am not a resume").unwrap();
        
        zip.start_file("nested/folder.pdf", options).unwrap();
        zip.write_all(b"%PDF-1.4 nested").unwrap();
        
        zip.finish().unwrap();
    }
    
    let zip_id = Uuid::new_v4();
    env.storage.put_object("zip-archives", "chaos.zip", buf, None).await.unwrap();
    sqlx::query!("INSERT INTO zip_archives (id, filename, status) VALUES ($1, $2, 'pending')", zip_id, "chaos.zip")
        .execute(&env.pool).await.unwrap();

    let payload = json!({ "record": { "id": zip_id, "filename": "chaos.zip" } });
    let token = create_jwt(&env.jwt_secret);
    
    let res = env.app.oneshot(Request::builder().method("POST").uri("/ingest/interns/batch")
        .header("Authorization", format!("Bearer {}", token))
        .header("Content-Type", "application/json")
        .body(Body::from(serde_json::to_vec(&payload).unwrap())).unwrap())
        .await.unwrap();

    assert_eq!(res.status(), StatusCode::ACCEPTED);

    // Poll for completion
    for _ in 0..10 {
        let rec = sqlx::query_as::<_, (DocumentStatus,)>("SELECT status FROM zip_archives WHERE id = $1").bind(zip_id).fetch_one(&env.pool).await.unwrap();
        if matches!(rec.0, DocumentStatus::Completed) { break; }
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    }

    {
        let objects = env.storage.objects.lock().unwrap();
        // Should have valid.pdf AND the nested one (it currently doesn't filter nested, just non-pdfs)
        assert!(objects.contains_key("resumes/chaos.zip_valid.pdf"));
        assert!(objects.contains_key("resumes/chaos.zip_nested/folder.pdf"));
        // Should NOT have the text file
        assert!(!objects.contains_key("resumes/chaos.zip_ignore_me.txt"));
    }
    sqlx::query!("DELETE FROM zip_archives WHERE id = $1", zip_id).execute(&env.pool).await.unwrap();
}

#[tokio::test]
async fn test_file_size_limit_exceeded() {
    let env = setup_test_env().await;
    let pdf_bytes = vec![0u8; 11 * 1024 * 1024]; // 11MB
    env.storage.put_object("resumes", "huge.pdf", pdf_bytes, None).await.unwrap();

    let upload_id = Uuid::new_v4();
    sqlx::query!("INSERT INTO resume_uploads (id, filename, status) VALUES ($1, $2, 'pending')", upload_id, "huge.pdf").execute(&env.pool).await.unwrap();

    let token = create_jwt(&env.jwt_secret);
    let payload = json!({ "record": { "id": upload_id, "filename": "huge.pdf" } });
    let res = env.app.oneshot(Request::builder().method("POST").uri("/ingest/interns/individual").header("Authorization", format!("Bearer {}", token)).header("Content-Type", "application/json").body(Body::from(serde_json::to_vec(&payload).unwrap())).unwrap()).await.unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);

    let mut success = false;
    for _ in 0..10 {
        let rec = sqlx::query_as::<_, (DocumentStatus,)>("SELECT status FROM resume_uploads WHERE id = $1").bind(upload_id).fetch_one(&env.pool).await.unwrap();
        if matches!(rec.0, DocumentStatus::Failed) { success = true; break; }
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    }
    assert!(success, "Large file should have failed");
}

#[tokio::test]
async fn test_zip_bomb_protection() {
    let env = setup_test_env().await;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for i in 0..505 {
            zip.start_file(format!("file{}.pdf", i), options).unwrap();
            zip.write_all(b"%PDF-1.4 ignored bytes").unwrap();
        }
        zip.finish().unwrap();
    }
    let zip_id = Uuid::new_v4();
    env.storage.put_object("zip-archives", "bomb.zip", buf, None).await.unwrap();
    sqlx::query!("INSERT INTO zip_archives (id, filename, status) VALUES ($1, $2, 'pending')", zip_id, "bomb.zip").execute(&env.pool).await.unwrap();

    let token = create_jwt(&env.jwt_secret);
    let payload = json!({ "record": { "id": zip_id, "filename": "bomb.zip" } });
    let res = env.app.oneshot(Request::builder().method("POST").uri("/ingest/interns/batch").header("Authorization", format!("Bearer {}", token)).header("Content-Type", "application/json").body(Body::from(serde_json::to_vec(&payload).unwrap())).unwrap()).await.unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);

    let mut success = false;
    for _ in 0..30 {
        let rec = sqlx::query_as::<_, (DocumentStatus,)>("SELECT status FROM zip_archives WHERE id = $1").bind(zip_id).fetch_one(&env.pool).await.unwrap();
        if matches!(rec.0, DocumentStatus::Failed) { success = true; break; }
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    }
    assert!(success, "Zip bomb should have failed");
}
