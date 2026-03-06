# Matchmaker Orchestrator

## Project Overview
This is a Rust-based backend service built with **Axum** designed to orchestrate the processing of resume files. It acts as a middleware between Supabase Storage (via S3 protocol), an OpenAI LLM, and a PostgreSQL database.

**Core Workflow:**
1.  Receives HTTP webhooks (single file, batch ZIP, or project spreadsheet).
2.  Downloads files from **Supabase Storage** using the **AWS S3 SDK**.
3.  Extracts "Term" metadata (e.g., "Spring 2026") from the storage path.
4.  Processes files:
    - **PDFs:** Extracts raw text and uses **OpenAI** to parse into structured JSON.
    - **ZIPs:** Extracts and re-uploads PDFs to term-specific subdirectories.
    - **Spreadsheets (CSV/XLSX):** Parses project data and inserts into the database.
5.  Updates records in **PostgreSQL** (via `sqlx`), tracking status, errors, and term lineage.

## Architecture
*   **Framework:** Axum (Web Server)
*   **Library First:** Core logic resides in `lib.rs`, allowing binary and test suite to share state and types.
*   **Runtime:** Tokio (Async)
*   **Database:** PostgreSQL (via `sqlx`)
*   **Storage Abstraction:** Uses `StorageProvider` trait to allow switching between real S3 and in-memory Mocks.
*   **AI Integration:** OpenAI API with configurable endpoints for testing.
*   **Job Tracking:** Automatic "Ready" state transitions and JSONB error aggregation in the `jobs` table.

## Key Files
*   `src/main.rs`: Entry point. Initializes `AppState` and launches the web server.
*   `src/lib.rs`: Library root. Defines `AppState` and modules.
*   `src/service.rs`: **Core Business Logic.** Handles PDF extraction, LLM orchestration, ZIP processing, and DB updates.
*   `src/storage.rs`: S3 and Mock storage implementations.
*   `src/config.rs`: Config parsing and S3 URL generation logic.
*   `tests/`: Comprehensive test suite (Integration and Logic).
*   `.github/workflows/ci.yml`: GitHub Actions configuration.

## Testing & Validation
The project uses a three-tier testing approach:
1.  **Unit Tests**: In-file tests for pure logic (parsers, config).
2.  **Logic Tests**: `tests/logic_tests.rs` for verifying complex SQL operations and state transitions.
3.  **Integration Tests**: `tests/integration_tests.rs` for full webhook-to-database flows using `wiremock` and `MockStorageProvider`.

**Run Tests:**
```bash
cargo test
```

## Local Development with Cloud
The project is currently linked to the **InternProjectMatchmaker** cloud project (`pkckwgszwgrvxwwdofcj`). 

*   **MCP Server:** The Supabase MCP server is configured to interact directly with this cloud environment.
*   **SQLx Offline:** Metadata in `.sqlx/` allows compilation without a live DB connection. Update via `cargo sqlx prepare -- --all-targets`.
*   **Webhooks:** Use a tool like **ngrok** to expose your local port (default `3000`) so that Supabase Cloud Webhooks can reach your local orchestrator.

## API Endpoints

### `POST /ingest/interns/individual`
Triggers processing for a single uploaded PDF.
*   **Payload:** JSON containing the file record (ID and filename).
*   **Behavior:** Spawns a background task via `ResumeService`. Returns HTTP 202 Accepted immediately.

### `POST /ingest/interns/batch`
Triggers processing for a ZIP archive of resumes.
*   **Payload:** JSON containing the file record.
*   **Behavior:** Spawns a background task via `ResumeService` to extract the ZIP and re-upload individual PDFs with `job_id` and `zip_id` metadata. Returns HTTP 202 Accepted.

### `POST /ingest/projects`
Triggers processing for a project spreadsheet (CSV or XLSX).
*   **Payload:** JSON containing the file record.
*   **Behavior:** Spawns a background task via `ProjectService` to parse rows, insert into the `projects` table, and check for job readiness. Returns HTTP 202 Accepted.

## Development Conventions
*   **State Management:** All shared state is held in `AppState` and injected via Axum's `State` extractor.
*   **Concurrency:** Uses `tokio::spawn` for background tasks, throttled by a `tokio::sync::Semaphore` (limit defined by `MAX_CONCURRENT_TASKS`) to prevent resource exhaustion.
*   **Database:** Uses `sqlx` with compile-time checked queries (mostly).
*   **Logging:** Uses structured logging via `tracing`. Failures in background tasks are logged as errors.
