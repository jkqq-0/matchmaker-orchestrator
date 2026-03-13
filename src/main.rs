use aws_config::Region;
use aws_sdk_s3::Client as S3Client;
use axum::{Router, routing::get, routing::post};
use dotenvy::dotenv;
use serde_json::Value;
use sqlx::postgres::PgPoolOptions;
use std::env;
use std::sync::Arc;
use tokio::sync::Semaphore;
use tower_http::trace::{self, TraceLayer};
use tracing::Level;

use matchmaker_orchestrator::AppState;
use matchmaker_orchestrator::auth;
use matchmaker_orchestrator::requests::{
    handle_batch_upload, handle_project_upload, handle_single_upload,
};
use matchmaker_orchestrator::storage::S3StorageProvider;

#[tokio::main]
async fn main() {
    dotenv().ok();
    let db_url = env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let endpoint = env::var("SUPABASE_ENDPOINT").expect("SUPABASE_ENDPOINT must be set");
    let service_key = env::var("SERVICE_KEY").expect("SERVICE_KEY must be set");
    let openai_api_key = env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY must be set");
    let max_concurrent_tasks = env::var("MAX_CONCURRENT_TASKS")
        .unwrap_or_else(|_| "10".to_string())
        .parse::<usize>()
        .expect("MAX_CONCURRENT_TASKS must be a number");

    let s3_config_parsed = matchmaker_orchestrator::config::parse_s3_config(&endpoint).expect("Failed to parse S3 config from endpoint");
    let s3_endpoint = s3_config_parsed.endpoint;
    let project_ref = s3_config_parsed.project_ref;

    tracing::info!("Configured S3 Endpoint: {}", s3_endpoint);
    tracing::info!("Project Ref: {}", project_ref);

    // Load and parse schema once
    let raw_schema_string = include_str!("resume_schema.json");
    let resume_schema: Value =
        serde_json::from_str(raw_schema_string).expect("Invalid JSON Schema File");

    tracing_subscriber::fmt()
        .with_target(false)
        .compact() // Use .json() here for production!
        .init();

    let s3_access_key = env::var("S3_ACCESS_KEY").unwrap_or_else(|_| project_ref.clone());
    let s3_secret_key = env::var("S3_SECRET_KEY").unwrap_or_else(|_| service_key.clone());

    // Configure AWS SDK for Supabase S3
    let credentials = aws_sdk_s3::config::Credentials::new(
        s3_access_key,
        s3_secret_key,
        None,
        None,
        "supabase-storage",
    );

    let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .region(Region::new("us-east-1")) // Region is required but ignored by Supabase
        .endpoint_url(&s3_endpoint)
        .credentials_provider(credentials)
        .load()
        .await;

    let s3_config = aws_sdk_s3::config::Builder::from(&config)
        .force_path_style(true)
        .build();

    let s3_client = S3Client::from_conf(s3_config);
    let storage = Arc::new(S3StorageProvider::new(s3_client));

    let pool = PgPoolOptions::new()
        .max_connections((max_concurrent_tasks + 5) as u32)
        .connect(&db_url)
        .await
        .unwrap();
    let http_client = reqwest::Client::new();
    let semaphore = Arc::new(Semaphore::new(max_concurrent_tasks));

    tracing::info!("Database connection established");

    let jwt_secret = auth::get_jwt_secret(&pool)
        .await
        .expect("Failed to get JWT Secret");

    let app_state = AppState {
        pool,
        storage,
        http_client,
        openai_api_key,
        openai_endpoint: "https://api.openai.com/v1/chat/completions".to_string(),
        resume_schema,
        semaphore,
        jwt_secret,
    };

    let protected_routes = Router::new()
        .route("/ingest/interns/individual", post(handle_single_upload))
        .route("/ingest/interns/batch", post(handle_batch_upload))
        .route("/ingest/projects", post(handle_project_upload))
        .route_layer(axum::middleware::from_fn_with_state(
            app_state.clone(),
            auth::auth,
        ));

    // Create the axum router
    let app = Router::new()
        .merge(protected_routes)
        .route("/hello-world", get(hello_world))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(trace::DefaultMakeSpan::new().level(Level::INFO))
                .on_response(
                    trace::DefaultOnResponse::new()
                        .level(Level::INFO)
                        .latency_unit(tower_http::LatencyUnit::Micros),
                ),
        )
        .with_state(app_state);

    // Define the IP and port listener (TCP)
    let address = "0.0.0.0:3000";
    let listener = tokio::net::TcpListener::bind(address).await.unwrap();
    tracing::info!("listening on {}", listener.local_addr().unwrap());

    // Call axum serve to launch the web server
    axum::serve(listener, app).await.unwrap();
}

async fn hello_world() -> &'static str {
    tracing::info!("hello-world handler accessed");
    "Hello, World!"
}
