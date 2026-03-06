# Plan: Orchestrator Testing Strategy

## Objective
Establish a multi-layered testing framework for the Matchmaker Orchestrator to ensure reliability, data integrity, and high-quality AI outputs. The strategy balances fast unit tests with comprehensive integration tests and specialized AI evaluations.

## 1. Testing Layers

### A. Unit Testing (Core Logic)
*   **Scope:** Pure functions and logic that do not depend on external services (S3, Postgres, OpenAI).
*   **Targets:**
    *   `ProjectService::parse_csv` and `ProjectService::parse_excel`.
    *   JSON schema validation logic.
    *   Metadata extraction from S3 paths (Term tracking).
*   **Goal:** 100% coverage of parsing edge cases (malformed files, missing headers, empty rows).

### B. Integration Testing (Infrastructure)
*   **Scope:** Verifying that the Axum handlers, Services, and Database work together.
*   **Tooling:**
    *   `sqlx::test`: Automatically manages test databases and transactions.
    *   `tower::ServiceExt`: Allows calling Axum routes in-memory without network overhead.
*   **Targets:**
    *   Webhook ingestion flow: `POST /ingest/...` -> DB record created with `pending`.
    *   Auth middleware: Ensuring `401 Unauthorized` for invalid JWTs.

### C. Mocking External Services
To keep tests fast and deterministic, we will mock third-party APIs:
*   **OpenAI API:** Use `wiremock` to simulate LLM responses (JSON) based on input text.
*   **Supabase Storage (S3):** Use a mock trait or `localstack` to simulate file uploads and downloads.

### D. Asynchronous "Poll-and-Verify"
Because the orchestrator uses background tasks (`tokio::spawn`), tests must account for eventual consistency.
*   **Pattern:**
    1.  Trigger an action (e.g., upload a ZIP).
    2.  Assert `202 Accepted`.
    3.  Poll the database status column (using a retry loop) until it hits `Completed` or `Failed`.
    4.  Assert the final state of the linked records.

### E. AI Evaluation & "Trust" Testing
Building on existing scripts and `plan_trust_test.md`:
*   **Gold Standard Evals:** Maintain a set of "perfect" resume-to-JSON mappings. Run periodic checks to ensure LLM prompt changes don't degrade parsing quality.
*   **Adversarial Testing:** Use "poisoned" data (as defined in the Trust Experiment) to see if the system or human reviewers catch blatant errors.

## 2. Implementation Roadmap

### Phase 1: Unit Test Foundation (Complete)
- [x] Refactor `ProjectService` parsers to associated functions.
- [x] Add unit tests for CSV parsing.
- [x] Add unit tests for Excel parsing (with validation of specific fields).

### Phase 2: Mocking & Integration (Complete)
- [x] Implement a `StorageProvider` trait and `MockStorageProvider` for S3 abstraction.
- [x] Set up `wiremock` for OpenAI in the test suite.
- [x] Create a `tests/integration_tests.rs` file for end-to-end flow verification.
    - [x] `test_project_upload_flow` (CSV/Excel -> DB)
    - [x] `test_resume_upload_flow` (PDF -> Mock LLM -> DB)
    - [x] `test_zip_upload_flow` (ZIP -> Extraction -> Re-upload)

### Phase 3: Automation
- [ ] Integrate `measure_wrongness.py` logic into a Rust-based benchmarker.
- [ ] Configure CI (GitHub Actions) to run `cargo test` on every push.

## 3. Success Metrics
*   **Zero Panics:** No malformed input file should ever cause a thread panic.
*   **Deterministic Failures:** If the LLM returns invalid JSON, the DB record MUST state `Failed` with a clear error message.
*   **Parsing Accuracy:** Maintain >95% similarity score against Gold Standard resume data.
