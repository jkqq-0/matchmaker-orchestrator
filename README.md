# Matchmaker Orchestrator

A Rust-based backend service built with **Axum** designed to orchestrate the processing of resume files. It acts as a middleware between Supabase Storage (via S3 protocol), an OpenAI LLM, and a PostgreSQL database.

## Core Workflow

1.  **Receive Webhooks:** Receives HTTP webhooks for single file uploads or batch ZIP archives.
2.  **Download:** Downloads files (PDFs or ZIPs) from **Supabase Storage** using the **AWS S3 SDK**.
3.  **Extract:** Extracts raw text from PDF files using `pdf-extract`.
4.  **Analyze:** Sends raw text to **OpenAI** to parse into a structured JSON format based on a predefined schema.
5.  **Persist:** Updates the corresponding record in **PostgreSQL** (via `sqlx`), tracking status (`pending`, `processing`, `completed`, `failed`) and lineage (linking resumes to their parent ZIP).

## Tech Stack

*   **Language:** Rust (Edition 2024)
*   **Web Framework:** [Axum](https://github.com/tokio-rs/axum)
*   **Async Runtime:** [Tokio](https://tokio.rs/)
*   **Database:** PostgreSQL with [sqlx](https://github.com/launchbadge/sqlx)
*   **Storage:** Supabase Storage (S3-compatible via `aws-sdk-s3`)
*   **AI:** OpenAI API for resume parsing
*   **Auth:** JWT validation for secure webhooks

## Project Structure

*   `src/main.rs`: Entry point. Initializes application state and sets up routes.
*   `src/lib.rs`: Library root. Exports modules and defines shared `AppState`.
*   `src/service.rs`: **Core Business Logic.** Handles PDF extraction, LLM orchestration, ZIP processing, and DB updates.
*   `src/storage.rs`: Abstraction layer for storage (S3 and Mock implementations).
*   `src/config.rs`: Pure logic for configuration parsing and URL construction.
*   `src/auth.rs`: JWT authentication middleware for protecting endpoints.
*   `src/requests/openai.rs`: OpenAI API integration helpers.
*   `tests/`: Integration and logic tests.
    *   `integration_tests.rs`: End-to-end webhook flow verification.
    *   `logic_tests.rs`: Deep-dive tests for SQL state machine and JSONB persistence.
    *   `schema.sql`: Database schema used for CI and local testing.

## Getting Started

### Prerequisites

*   Rust (latest stable)
*   PostgreSQL database (or Supabase project)
*   OpenAI API Key

### Configuration

Create a `.env` file in the root directory:

```env
DATABASE_URL=postgres://postgres.[PROJ_REF]:[PASS]@aws-1-us-east-2.pooler.supabase.com:5432/postgres
SUPABASE_ENDPOINT=https://your-project.supabase.co
SERVICE_KEY=your-supabase-service-role-key
OPENAI_API_KEY=your-openai-api-key
MAX_CONCURRENT_TASKS=10
```

### Testing

The project includes a comprehensive testing suite.

```bash
# Run all tests (Unit, Logic, and Integration)
cargo test

# Run tests with detailed tracing output
cargo test -- --nocapture
```

Note: Integration tests require a `DATABASE_URL` to be set in your `.env` file. They use a "poll-and-verify" pattern to check background task completion.

### CI/CD

A GitHub Actions pipeline is configured in `.github/workflows/ci.yml`. It automatically:
1. Spins up a Postgres service.
2. Initializes the schema using `tests/schema.sql`.
3. Runs the full test suite on every push to `main` or `dev`.

### Running the Application

```bash
# Run development server
cargo run

# Build for production
cargo build --release
```

The server listens on `0.0.0.0:3000` by default.

## API Endpoints

The first three endpoints are not called manually, but are activated by a Supabase webhook when the relevant file is uploaded to the right storage bucket.

### `POST /ingest/interns/individual`
Processes a single uploaded PDF.
*   **Payload:** JSON with file ID and filename.
*   **Response:** `202 Accepted` (processing continues in background).

### `POST /ingest/interns/batch`
Processes a ZIP archive containing multiple resumes.
*   **Payload:** JSON with file ID and filename.
*   **Response:** `202 Accepted`.

### `POST /ingest/projects`
Processes a .csv or .xlsx spreadsheet file with project data.
* **Payload:** JSON with file ID and filename
* **Response:** `202 Accepted`.

### `GET /hello-world`
Basic test endpoint.
* **Response:** `Hello, World!`

## Development

### Concurrency
The application uses `tokio::spawn` for background tasks, throttled by a `tokio::sync::Semaphore` to prevent resource exhaustion. The limit is configurable via `MAX_CONCURRENT_TASKS`.

### Database
Queries are managed with `sqlx`, ensuring compile-time safety for most database interactions.

### Logging
Structured logging is implemented via `tracing` and `tracing-subscriber`.
