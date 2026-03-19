//! Load test for the matchmaker-orchestrator `/ingest/interns/individual` endpoint.
//!
//! Spins up a real Axum server with a MockStorageProvider and a wiremock OpenAI stub,
//! fires N concurrent HTTP requests, then polls the local DB until all background
//! tasks reach a terminal state. Reports p50/p95/p99 latencies for both the HTTP
//! acknowledgment and the full end-to-end processing time.
//!
//! # Prerequisites
//!   1. Local Supabase running (`supabase start`)
//!   2. Webhook triggers disabled (`psql ... -f scripts/disable_local_triggers.sql`)
//!
//! # Usage
//!   LOAD_TEST_DATABASE_URL="postgresql://postgres:postgres@127.0.0.1:54322/postgres" \
//!   cargo run --example load_test
//!
//! # Environment Variables
//!   LOAD_TEST_DATABASE_URL   - Required. Local Postgres connection string.
//!   LOAD_TEST_REQUESTS       - Total requests to fire. Default: 100
//!   LOAD_TEST_HTTP_CONCURRENCY - Max in-flight HTTP requests. Default: 50
//!   LOAD_TEST_TASK_CONCURRENCY - AppState semaphore size (background task limit). Default: 10

use axum::{Router, routing::post};
use jsonwebtoken::{EncodingKey, Header, encode};
use matchmaker_orchestrator::auth::{self, Claims};
use matchmaker_orchestrator::requests::handle_single_upload;
use matchmaker_orchestrator::storage::{MockStorageProvider, StorageProvider};
use matchmaker_orchestrator::AppState;
use serde_json::json;
use sqlx::Row;
use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// All N test uploads share one PDF key in MockStorage. The mock just returns
// the same bytes for every get_object call, so this is fine.
const TEST_PDF_KEY: &str = "load_test_resume.pdf";

// Minimal canned OpenAI response — fast, deterministic, always valid JSON.
const FAKE_OPENAI_RESPONSE: &str = r#"{
  "choices": [{
    "message": {
      "role": "assistant",
      "content": "{\"name\":\"Load Test User\",\"skills\":[\"Rust\",\"Testing\"]}"
    }
  }]
}"#;

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

struct Config {
    database_url: String,
    /// Total number of HTTP requests to fire.
    total_requests: usize,
    /// Max in-flight HTTP requests at any time (controls client-side concurrency).
    http_concurrency: usize,
    /// AppState semaphore size — limits concurrent background processing tasks
    /// on the server. Set lower than http_concurrency to observe backpressure.
    task_concurrency: usize,
    /// JWT secret for the local Supabase instance.
    /// Local Supabase default: "super-secret-jwt-token-with-at-least-32-characters-long"
    jwt_secret: String,
}

fn parse_config() -> Config {
    dotenvy::dotenv().ok(); // Load .env if present (won't override shell env)
    Config {
        database_url: std::env::var("LOAD_TEST_DATABASE_URL").expect(
            "LOAD_TEST_DATABASE_URL must be set.\n\
             Example: postgresql://postgres:postgres@127.0.0.1:54322/postgres",
        ),
        total_requests: std::env::var("LOAD_TEST_REQUESTS")
            .unwrap_or_else(|_| "100".to_string())
            .parse()
            .expect("LOAD_TEST_REQUESTS must be a positive integer"),
        http_concurrency: std::env::var("LOAD_TEST_HTTP_CONCURRENCY")
            .unwrap_or_else(|_| "50".to_string())
            .parse()
            .expect("LOAD_TEST_HTTP_CONCURRENCY must be a positive integer"),
        task_concurrency: std::env::var("LOAD_TEST_TASK_CONCURRENCY")
            .unwrap_or_else(|_| "10".to_string())
            .parse()
            .expect("LOAD_TEST_TASK_CONCURRENCY must be a positive integer"),
        // Local Supabase ships with this well-known default secret.
        // Override with LOAD_TEST_JWT_SECRET if your local instance differs.
        jwt_secret: std::env::var("LOAD_TEST_JWT_SECRET").unwrap_or_else(|_| {
            "super-secret-jwt-token-with-at-least-32-characters-long".to_string()
        }),
    }
}

// ---------------------------------------------------------------------------
// JWT helpers
// ---------------------------------------------------------------------------

fn make_jwt(secret: &str) -> String {
    let claims = Claims {
        sub: "load-test-user".to_string(),
        exp: 9_999_999_999,
        aud: None,
        role: Some("authenticated".to_string()),
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .expect("Failed to encode JWT")
}

// ---------------------------------------------------------------------------
// Statistics helpers
// ---------------------------------------------------------------------------

/// Returns the value at the given percentile from a pre-sorted slice.
fn percentile(sorted: &[Duration], pct: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let idx = ((pct / 100.0) * (sorted.len() as f64 - 1.0)).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn fmt_ms(d: Duration) -> String {
    format!("{:>7.1}ms", d.as_secs_f64() * 1000.0)
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    // Keep logging minimal so it doesn't pollute the results output.
    // Errors from background tasks will still appear on stderr.
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let config = parse_config();

    println!("╔══════════════════════════════════════════╗");
    println!("║   Matchmaker Orchestrator  Load Test     ║");
    println!("╠══════════════════════════════════════════╣");
    println!("  Requests:         {}", config.total_requests);
    println!("  HTTP concurrency: {}", config.http_concurrency);
    println!("  Task concurrency: {} (AppState semaphore)", config.task_concurrency);
    println!();

    // ── 1. Database ──────────────────────────────────────────────────────────
    print!("Connecting to database... ");
    std::io::stdout().flush().ok();
    let pool = sqlx::PgPool::connect(&config.database_url)
        .await
        .expect("Failed to connect to database. Is local Supabase running?\nRun: supabase start");
    println!("OK");

    // ── 2. Mock Storage ──────────────────────────────────────────────────────
    // Seed ONE PDF under TEST_PDF_KEY. All N upload rows reference this key,
    // and MockStorageProvider will return the same bytes for each get_object.
    let storage = Arc::new(MockStorageProvider::new());
    let pdf_bytes = std::fs::read("archive.zip-resumes/Alex_Rivera_CV.pdf")
        .expect("Test PDF not found. Run from the project root directory.");
    storage
        .put_object(
            "resumes",
            TEST_PDF_KEY,
            pdf_bytes,
            None::<std::collections::HashMap<String, String>>,
        )
        .await
        .expect("Failed to seed mock storage");
    println!("Mock storage seeded with test PDF ({}).", TEST_PDF_KEY);

    // ── 3. Wiremock OpenAI Stub ──────────────────────────────────────────────
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(FAKE_OPENAI_RESPONSE)
                .append_header("Content-Type", "application/json"),
        )
        .mount(&mock_server)
        .await;
    println!("OpenAI stub:        {}", mock_server.uri());

    // ── 4. Build AppState and start Axum server ──────────────────────────────
    let app_state = AppState {
        pool: pool.clone(),
        storage: storage.clone(),
        http_client: reqwest::Client::new(),
        openai_api_key: "load-test-key".to_string(),
        openai_endpoint: mock_server.uri(),
        resume_schema: json!({}),
        // This semaphore is the bottleneck we're testing. Set it lower than
        // http_concurrency to observe queuing/backpressure behavior.
        semaphore: Arc::new(Semaphore::new(config.task_concurrency)),
        jwt_secret: config.jwt_secret.clone(),
    };

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("Failed to bind listener");
    let server_addr = listener.local_addr().unwrap();
    println!("Axum server:        http://{}", server_addr);
    println!();

    let app = Router::new()
        .route("/ingest/interns/individual", post(handle_single_upload))
        .route_layer(axum::middleware::from_fn_with_state(
            app_state.clone(),
            auth::auth,
        ))
        .with_state(app_state);

    tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("Axum server crashed");
    });

    // ── 5. Pre-insert resume_upload rows ─────────────────────────────────────
    // Each HTTP request references a pre-existing row via its UUID.
    let upload_ids: Vec<Uuid> = (0..config.total_requests)
        .map(|_| Uuid::new_v4())
        .collect();

    print!(
        "Inserting {} resume_upload rows... ",
        config.total_requests
    );
    std::io::stdout().flush().ok();
    for &id in &upload_ids {
        sqlx::query(
            "INSERT INTO resume_uploads (id, filename, status) VALUES ($1, $2, 'pending')",
        )
        .bind(id)
        .bind(TEST_PDF_KEY)
        .execute(&pool)
        .await
        .expect("Failed to insert resume_upload row");
    }
    println!("done.");

    // ── 6. Fire requests ─────────────────────────────────────────────────────
    let token = make_jwt(&config.jwt_secret);
    let http_client = reqwest::Client::new();
    let http_sem = Arc::new(Semaphore::new(config.http_concurrency));
    let endpoint = format!("http://{}/ingest/interns/individual", server_addr);

    println!(
        "Firing {} requests (http_concurrency={})...",
        config.total_requests, config.http_concurrency
    );
    let overall_start = Instant::now();

    let mut task_handles = Vec::with_capacity(config.total_requests);
    for &upload_id in &upload_ids {
        let client = http_client.clone();
        let token = token.clone();
        let url = endpoint.clone();
        let sem = http_sem.clone();

        task_handles.push(tokio::spawn(async move {
            // acquire_owned() avoids a lifetime tie to the semaphore Arc
            let _permit = sem.acquire_owned().await.unwrap();
            let request_start = Instant::now();

            let payload = json!({
                "record": {
                    "id": upload_id,
                    "filename": TEST_PDF_KEY
                }
            });

            let res = client
                .post(&url)
                .bearer_auth(&token)
                .json(&payload)
                .send()
                .await;

            let ack_latency = request_start.elapsed();

            match res {
                Ok(r) if r.status().as_u16() == 202 => {
                    (upload_id, true, Some(ack_latency), request_start)
                }
                Ok(r) => {
                    eprintln!("  Unexpected HTTP {} for upload {}", r.status(), upload_id);
                    (upload_id, false, None, request_start)
                }
                Err(e) => {
                    eprintln!("  Request error for {}: {}", upload_id, e);
                    (upload_id, false, None, request_start)
                }
            }
        }));
    }

    // Collect HTTP results
    let mut ack_latencies: Vec<Duration> = Vec::with_capacity(config.total_requests);
    // Maps upload_id -> Instant when its HTTP request was sent (for e2e timing)
    let mut request_starts: HashMap<Uuid, Instant> = HashMap::with_capacity(config.total_requests);
    let mut http_errors = 0usize;

    for handle in task_handles {
        let (id, ok, ack_latency, start) = handle.await.unwrap();
        if ok {
            if let Some(lat) = ack_latency {
                ack_latencies.push(lat);
            }
            request_starts.insert(id, start);
        } else {
            http_errors += 1;
        }
    }

    println!(
        "  All 202s received in {:.2}s ({} HTTP errors)",
        overall_start.elapsed().as_secs_f64(),
        http_errors
    );

    // ── 7. Poll DB for completion ─────────────────────────────────────────────
    println!("Waiting for background tasks to complete...");

    // pending maps: upload_id -> Instant when its HTTP request was sent
    let mut pending: HashMap<Uuid, Instant> = request_starts;
    let mut e2e_latencies: Vec<Duration> = Vec::with_capacity(pending.len());
    let mut completed_count = 0usize;
    let mut failed_count = 0usize;

    let poll_start = Instant::now();
    let timeout = Duration::from_secs(300);

    loop {
        if pending.is_empty() {
            break;
        }
        if poll_start.elapsed() > timeout {
            eprintln!(
                "\nTimeout — {} tasks never reached a terminal state.",
                pending.len()
            );
            break;
        }

        tokio::time::sleep(Duration::from_millis(250)).await;

        let ids: Vec<Uuid> = pending.keys().cloned().collect();

        // Single query for all pending IDs that are now terminal
        let rows = sqlx::query(
            "SELECT id, status::text AS status \
             FROM resume_uploads \
             WHERE id = ANY($1) AND status IN ('completed', 'failed')",
        )
        .bind(&ids)
        .fetch_all(&pool)
        .await
        .expect("Failed to poll resume_uploads");

        for row in rows {
            let id: Uuid = row.get("id");
            let status: String = row.get("status");
            if let Some(start) = pending.remove(&id) {
                e2e_latencies.push(start.elapsed());
                if status == "completed" {
                    completed_count += 1;
                } else {
                    failed_count += 1;
                }
            }
        }

        // Inline progress indicator (overwrites same line)
        let done = config.total_requests - pending.len() - http_errors;
        print!(
            "\r  {}/{} done  ({} pending)   ",
            done,
            config.total_requests - http_errors,
            pending.len()
        );
        std::io::stdout().flush().ok();
    }

    println!(); // newline after the progress line
    let total_elapsed = overall_start.elapsed();

    // ── 8. Report ─────────────────────────────────────────────────────────────
    ack_latencies.sort();
    e2e_latencies.sort();

    println!();
    println!("╔══════════════════════════════════════════╗");
    println!("║                 Results                  ║");
    println!("╠══════════════════════════════════════════╣");
    println!(
        "  Total wall time:  {:.2}s",
        total_elapsed.as_secs_f64()
    );
    println!(
        "  Throughput:       {:.1} req/s",
        config.total_requests as f64 / total_elapsed.as_secs_f64()
    );

    println!();
    println!("  ── HTTP 202 Acknowledgment Latency ──────────────");
    println!(
        "     (time from request send → 202 response received)");
    println!(
        "     p50: {}  p95: {}  p99: {}",
        fmt_ms(percentile(&ack_latencies, 50.0)),
        fmt_ms(percentile(&ack_latencies, 95.0)),
        fmt_ms(percentile(&ack_latencies, 99.0)),
    );

    println!();
    println!("  ── End-to-End Processing Latency ────────────────");
    println!(
        "     (time from request send → DB row reaches terminal state)");
    println!(
        "     p50: {}  p95: {}  p99: {}",
        fmt_ms(percentile(&e2e_latencies, 50.0)),
        fmt_ms(percentile(&e2e_latencies, 95.0)),
        fmt_ms(percentile(&e2e_latencies, 99.0)),
    );

    println!();
    println!("  ── Outcomes ─────────────────────────────────────");
    println!("     Completed:    {}", completed_count);
    println!("     Failed:       {}", failed_count);
    println!("     HTTP errors:  {}", http_errors);
    if !pending.is_empty() {
        println!("     Timed out:    {} (never reached terminal state)", pending.len());
    }

    // ── 9. Cleanup ────────────────────────────────────────────────────────────
    println!();
    print!("Cleaning up test data... ");
    std::io::stdout().flush().ok();

    // Resumes created by background tasks that completed successfully
    sqlx::query("DELETE FROM resumes WHERE upload_id = ANY($1)")
        .bind(&upload_ids)
        .execute(&pool)
        .await
        .expect("Cleanup failed: resumes");

    sqlx::query("DELETE FROM resume_uploads WHERE id = ANY($1)")
        .bind(&upload_ids)
        .execute(&pool)
        .await
        .expect("Cleanup failed: resume_uploads");

    println!("done.");
    println!("╚══════════════════════════════════════════╝");
}
