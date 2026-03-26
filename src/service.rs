use crate::AppState;
use crate::requests::openai::generate_structure_from_pdf;
use calamine::{DataType, Reader, Xlsx, open_workbook_from_rs};
use csv::ReaderBuilder;
use serde_json::{Value, from_str};
use std::io::{Cursor, Read, Write};
use uuid::Uuid;

#[derive(Debug, sqlx::Type, serde::Serialize, serde::Deserialize)]
#[sqlx(type_name = "document_status", rename_all = "lowercase")]
pub enum DocumentStatus {
    Pending,
    Processing,
    Completed,
    Failed,
}

#[derive(Debug, sqlx::Type, serde::Serialize, serde::Deserialize, Clone, Copy, PartialEq, Eq)]
#[sqlx(type_name = "job_status", rename_all = "lowercase")]
pub enum JobStatus {
    Pending,
    Processing,
    Ready,
    Matching,
    Completed,
    Failed,
}

const MAX_PDF_SIZE: usize = 10 * 1024 * 1024; // 10 MB
const MAX_ZIP_ARCHIVE_SIZE: usize = 50 * 1024 * 1024; // 50 MB
const MAX_UNCOMPRESSED_TOTAL_SIZE: u64 = 100 * 1024 * 1024; // 100 MB
const MAX_ZIP_FILES: usize = 500;

pub struct ResumeService {
    state: AppState,
}

impl ResumeService {
    pub fn new(state: AppState) -> Self {
        Self { state }
    }

    async fn update_resume_upload_status(
        &self,
        id: Uuid,
        status: DocumentStatus,
        error_message: Option<String>,
    ) -> Result<(), sqlx::Error> {
        sqlx::query!(
            "UPDATE resume_uploads SET status = $1, error_message = $2 WHERE id = $3",
            status as DocumentStatus,
            error_message,
            id
        )
        .execute(&self.state.pool)
        .await?;
        Ok(())
    }

    async fn record_job_error(&self, job_id: Uuid, resume_id: Uuid, error: String) {
        let error_json = serde_json::json!({
            "id": resume_id,
            "error": error
        });

        let _ = sqlx::query!(
            "UPDATE jobs SET rust_error = jsonb_set(rust_error, '{resumes}', rust_error->'resumes' || $1) WHERE id = $2",
            error_json,
            job_id
        )
        .execute(&self.state.pool)
        .await;
    }

    async fn maybe_mark_job_as_ready(&self, job_id: Uuid) {
        // Check if all related uploads are in a terminal state
        let pending_count = match sqlx::query!(
            r#"
            SELECT 
                (SELECT count(*) FROM resume_uploads WHERE job_id = $1 AND status NOT IN ('completed', 'failed')) +
                (SELECT count(*) FROM zip_archives WHERE job_id = $1 AND status NOT IN ('completed', 'failed')) +
                (SELECT count(*) FROM project_uploads WHERE job_id = $1 AND status NOT IN ('completed', 'failed'))
            as count
            "#,
            job_id
        )
        .fetch_one(&self.state.pool)
        .await {
            Ok(r) => r.count.unwrap_or(0),
            Err(_) => return,
        };

        if pending_count == 0 {
            let _ = sqlx::query!(
                "UPDATE jobs SET status = $1 WHERE id = $2 AND status = $3",
                JobStatus::Ready as JobStatus,
                job_id,
                JobStatus::Processing as JobStatus
            )
            .execute(&self.state.pool)
            .await;
        }
    }

    pub async fn process_resume_upload(&self, upload_id: Uuid, filename: String) {
        let _permit = self
            .state
            .semaphore
            .acquire()
            .await
            .expect("Semaphore closed");

        // Fetch details from resume_uploads
        let upload_record = match sqlx::query!(
            "SELECT user_id, term, zip_id, job_id FROM resume_uploads WHERE id = $1",
            upload_id
        )
        .fetch_one(&self.state.pool)
        .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("Failed to fetch resume upload record: {}", e);
                return;
            }
        };

        // Mark upload as processing
        let _ = self
            .update_resume_upload_status(upload_id, DocumentStatus::Processing, None)
            .await;

        // Mark job as processing if it was pending
        if let Some(job_id) = upload_record.job_id {
            let _ = sqlx::query!(
                "UPDATE jobs SET status = $1 WHERE id = $2 AND status = $3",
                JobStatus::Processing as JobStatus,
                job_id,
                JobStatus::Pending as JobStatus
            )
            .execute(&self.state.pool)
            .await;
        }

        // Download
        let pdf_data = match self
            .state
            .storage
            .get_object("resumes", &filename, Some(MAX_PDF_SIZE))
            .await
        {
            Ok(data) => data,
            Err(e) => {
                let err_msg = format!("Failed to download pdf: {}", e);
                tracing::error!(
                    "{}, filename {}, upload_id {}",
                    err_msg,
                    filename,
                    upload_id
                );
                let _ = self
                    .update_resume_upload_status(
                        upload_id,
                        DocumentStatus::Failed,
                        Some(err_msg.clone()),
                    )
                    .await;
                if let Some(job_id) = upload_record.job_id {
                    self.record_job_error(job_id, upload_id, err_msg).await;
                }
                return;
            }
        };

        // Create resume record
        let resume_id = Uuid::new_v4();
        if let Err(e) = sqlx::query!(
            "INSERT INTO resumes (id, user_id, filename, term, zip_id, upload_id) VALUES ($1, $2, $3, $4, $5, $6)",
            resume_id,
            upload_record.user_id,
            filename,
            upload_record.term,
            upload_record.zip_id,
            upload_id
        )
        .execute(&self.state.pool)
        .await {
            let err_msg = format!("Failed to create resume record: {}", e);
            tracing::error!("{}", err_msg);
            let _ = self.update_resume_upload_status(upload_id, DocumentStatus::Failed, Some(err_msg.clone())).await;
            if let Some(job_id) = upload_record.job_id {
                self.record_job_error(job_id, upload_id, err_msg).await;
            }
            return;
        }

        // Parse and process
        match self
            .process_single_pdf(&pdf_data, &filename, resume_id)
            .await
        {
            Some((pdf_text, parsed_json)) => {
                match self
                    .update_resume_record(resume_id, pdf_text, parsed_json)
                    .await
                {
                    Ok(_) => {
                        tracing::info!(
                            "Resume record {} (filename: {}) updated successfully",
                            resume_id,
                            filename
                        );
                        let _ = self
                            .update_resume_upload_status(upload_id, DocumentStatus::Completed, None)
                            .await;
                    }
                    Err(e) => {
                        let err_msg = format!("Failed to update database record: {}", e);
                        tracing::error!(
                            "{}, filename {}, resume_id {}",
                            err_msg,
                            filename,
                            resume_id
                        );
                        let _ = self
                            .update_resume_upload_status(
                                upload_id,
                                DocumentStatus::Failed,
                                Some(err_msg.clone()),
                            )
                            .await;
                        if let Some(job_id) = upload_record.job_id {
                            self.record_job_error(job_id, upload_id, err_msg).await;
                        }
                    }
                }
            }
            None => {
                let err_msg = "PDF processing or LLM parsing failed".to_string();
                tracing::warn!(
                    "{}, filename {}, resume_id {}",
                    err_msg,
                    filename,
                    resume_id
                );
                let _ = self
                    .update_resume_upload_status(
                        upload_id,
                        DocumentStatus::Failed,
                        Some(err_msg.clone()),
                    )
                    .await;
                if let Some(job_id) = upload_record.job_id {
                    self.record_job_error(job_id, upload_id, err_msg).await;
                }
            }
        }

        // Finalize job status if this was the last part
        if let Some(job_id) = upload_record.job_id {
            self.maybe_mark_job_as_ready(job_id).await;
        }
    }

    pub async fn process_single_pdf(
        &self,
        pdf_data: &[u8],
        filename: &str,
        id: Uuid,
    ) -> Option<(String, Value)> {
        let pdf_text = match pdf_extract::extract_text_from_mem(pdf_data) {
            Ok(text) => text,
            Err(e) => {
                tracing::error!(
                    "Failed to extract text from PDF for filename {}, id {}: {}",
                    filename,
                    id,
                    e
                );
                return None;
            }
        };

        let response = generate_structure_from_pdf(
            &pdf_text,
            &self.state.http_client,
            &self.state.openai_api_key,
            &self.state.openai_endpoint,
            &self.state.resume_schema,
        )
        .await;
        match response {
            Ok(r) => {
                if let Some(choice) = r.choices.first() {
                    let raw_content = &choice.message.content;
                    match from_str::<Value>(raw_content) {
                        Ok(parsed_json) => {
                            tracing::info!(
                                "LLM-generated JSON received for filename {}, id {}",
                                filename,
                                id
                            );
                            Some((pdf_text, parsed_json))
                        }
                        Err(e) => {
                            tracing::error!(
                                "LLM returned invalid JSON for filename {}, id {}: {}",
                                filename,
                                id,
                                e
                            );
                            None
                        }
                    }
                } else {
                    tracing::error!(
                        "No choices returned from LLM for filename {}, id {}",
                        filename,
                        id
                    );
                    None
                }
            }
            Err(e) => {
                tracing::error!(
                    "LLM request failed for filename {}, id {}: {:#?}",
                    filename,
                    id,
                    e
                );
                None
            }
        }
    }

    pub async fn update_resume_record(
        &self,
        id: Uuid,
        text: String,
        structured_json: Value,
    ) -> Result<sqlx::postgres::PgQueryResult, sqlx::Error> {
        sqlx::query!(
            "UPDATE resumes SET text = $1, structured = $2 WHERE id = $3",
            text,
            structured_json,
            id
        )
        .execute(&self.state.pool)
        .await
    }

    async fn update_zip_status(
        &self,
        id: Uuid,
        status: DocumentStatus,
        error_message: Option<String>,
    ) -> Result<(), sqlx::Error> {
        sqlx::query!(
            "UPDATE zip_archives SET status = $1, error_message = $2 WHERE id = $3",
            status as DocumentStatus,
            error_message,
            id
        )
        .execute(&self.state.pool)
        .await?;
        Ok(())
    }

    pub async fn handle_batch_extraction(&self, id: Uuid, filename: String) {
        let _permit = self
            .state
            .semaphore
            .acquire()
            .await
            .expect("Semaphore closed");

        // Fetch job_id
        let zip_record = match sqlx::query!("SELECT job_id FROM zip_archives WHERE id = $1", id)
            .fetch_one(&self.state.pool)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("Failed to fetch zip record: {}", e);
                return;
            }
        };

        // Mark as processing
        let _ = self
            .update_zip_status(id, DocumentStatus::Processing, None)
            .await;

        // Mark job as processing
        if let Some(job_id) = zip_record.job_id {
            let _ = sqlx::query!(
                "UPDATE jobs SET status = $1 WHERE id = $2 AND status = $3",
                JobStatus::Processing as JobStatus,
                job_id,
                JobStatus::Pending as JobStatus
            )
            .execute(&self.state.pool)
            .await;
        }

        // Download
        let zip_data = match self
            .state
            .storage
            .get_object("zip-archives", &filename, Some(MAX_ZIP_ARCHIVE_SIZE))
            .await
        {
            Ok(data) => data,
            Err(e) => {
                let err_msg = format!("Failed to download zip: {}", e);
                tracing::error!("{}, filename {}, id {}", err_msg, filename, id);
                let _ = self
                    .update_zip_status(id, DocumentStatus::Failed, Some(err_msg))
                    .await;
                return;
            }
        };

        let mut tmp_file = match tempfile::tempfile() {
            Ok(f) => f,
            Err(e) => {
                let err_msg = format!("Failed to create tempfile: {}", e);
                tracing::error!("{}", err_msg);
                let _ = self
                    .update_zip_status(id, DocumentStatus::Failed, Some(err_msg))
                    .await;
                return;
            }
        };

        if let Err(e) = tmp_file.write_all(&zip_data) {
            let err_msg = format!("Failed to write to tempfile: {}", e);
            tracing::error!("{}", err_msg);
            let _ = self
                .update_zip_status(id, DocumentStatus::Failed, Some(err_msg))
                .await;
            return;
        }

        let mut archive = match zip::ZipArchive::new(tmp_file) {
            Ok(a) => a,
            Err(e) => {
                let err_msg = format!("Failed to create zip archive: {}", e);
                tracing::error!("{}", err_msg);
                let _ = self
                    .update_zip_status(id, DocumentStatus::Failed, Some(err_msg))
                    .await;
                return;
            }
        };

        tracing::info!(
            "Successfully opened zip archive with {} files",
            archive.len()
        );

        let mut extracted_files_count = 0;
        let mut total_extracted_size: u64 = 0;

        for i in 0..archive.len() {
            let mut file = match archive.by_index(i) {
                Ok(f) => f,
                Err(e) => {
                    tracing::error!("Failed to read file at index {} in zip: {}", i, e);
                    continue;
                }
            };

            if file.is_dir() || !file.name().ends_with(".pdf") || file.name().starts_with('.') {
                continue;
            }

            extracted_files_count += 1;
            if extracted_files_count > MAX_ZIP_FILES {
                let err_msg = format!(
                    "Zip bomb detected: Exceeded max file count of {}",
                    MAX_ZIP_FILES
                );
                tracing::error!("{}", err_msg);
                let _ = self
                    .update_zip_status(id, DocumentStatus::Failed, Some(err_msg))
                    .await;
                return;
            }

            let uncompressed_size = file.size();
            total_extracted_size = total_extracted_size.saturating_add(uncompressed_size);

            if total_extracted_size > MAX_UNCOMPRESSED_TOTAL_SIZE {
                let err_msg = format!(
                    "Zip bomb detected: Exceeded max uncompressed size of {} bytes",
                    MAX_UNCOMPRESSED_TOTAL_SIZE
                );
                tracing::error!("{}", err_msg);
                let _ = self
                    .update_zip_status(id, DocumentStatus::Failed, Some(err_msg))
                    .await;
                return;
            }

            if uncompressed_size > MAX_PDF_SIZE as u64 {
                tracing::warn!(
                    "Skipping file {} inside zip because it exceeds MAX_PDF_SIZE",
                    file.name()
                );
                continue;
            }

            // Read the decompressed file safely since size is bounded
            let mut pdf_buffer = Vec::with_capacity(uncompressed_size as usize);
            if let Err(e) = file.read_to_end(&mut pdf_buffer) {
                tracing::error!("Failed to read file {} to buffer: {}", file.name(), e);
                continue;
            }

            let pdf_name = file.name().to_string();
            let upload_path = format!("{}_{}", filename, pdf_name);

            let storage = self.state.storage.clone();
            let semaphore = self.state.semaphore.clone();
            let zip_id_str = id.to_string();
            let job_id = zip_record.job_id;

            tokio::spawn(async move {
                let _permit = semaphore.acquire().await.expect("Semaphore closed");

                let mut metadata = std::collections::HashMap::new();
                metadata.insert("zip_id".to_string(), zip_id_str);
                if let Some(j_id) = job_id {
                    metadata.insert("job_id".to_string(), j_id.to_string());
                }

                match storage
                    .put_object("resumes", &upload_path, pdf_buffer, Some(metadata))
                    .await
                {
                    Ok(_) => {
                        tracing::info!("Successfully re-uploaded extracted PDF: {}", upload_path)
                    }
                    Err(e) => {
                        tracing::error!("Failed to upload extracted PDF {}: {}", upload_path, e)
                    }
                }
            });
        }

        let _ = self
            .update_zip_status(id, DocumentStatus::Completed, None)
            .await;

        if let Some(job_id) = zip_record.job_id {
            self.maybe_mark_job_as_ready(job_id).await;
        }
    }
}

pub struct ProjectService {
    state: AppState,
}

impl ProjectService {
    pub fn new(state: AppState) -> Self {
        Self { state }
    }

    pub async fn update_upload_status(
        &self,
        id: Uuid,
        status: DocumentStatus,
        error_message: Option<String>,
    ) -> Result<(), sqlx::Error> {
        sqlx::query!(
            "UPDATE project_uploads SET status = $1, error_message = $2 WHERE id = $3",
            status as DocumentStatus,
            error_message,
            id
        )
        .execute(&self.state.pool)
        .await?;
        Ok(())
    }

    pub async fn record_job_error(&self, job_id: Uuid, project_id: Uuid, error: String) {
        let error_json = serde_json::json!({
            "id": project_id,
            "error": error
        });

        let _ = sqlx::query!(
            "UPDATE jobs SET rust_error = jsonb_set(rust_error, '{projects}', rust_error->'projects' || $1) WHERE id = $2",
            error_json,
            job_id
        )
        .execute(&self.state.pool)
        .await;
    }

    pub async fn maybe_mark_job_as_ready(&self, job_id: Uuid) {
        // Reuse logic or consolidate. For now, duplication is simpler than refactoring everything to a shared trait.
        let pending_count = match sqlx::query!(
            r#"
            SELECT 
                (SELECT count(*) FROM resume_uploads WHERE job_id = $1 AND status NOT IN ('completed', 'failed')) +
                (SELECT count(*) FROM zip_archives WHERE job_id = $1 AND status NOT IN ('completed', 'failed')) +
                (SELECT count(*) FROM project_uploads WHERE job_id = $1 AND status NOT IN ('completed', 'failed'))
            as count
            "#,
            job_id
        )
        .fetch_one(&self.state.pool)
        .await {
            Ok(r) => r.count.unwrap_or(0),
            Err(_) => return,
        };

        if pending_count == 0 {
            let _ = sqlx::query!(
                "UPDATE jobs SET status = $1 WHERE id = $2 AND status = $3",
                JobStatus::Ready as JobStatus,
                job_id,
                JobStatus::Processing as JobStatus
            )
            .execute(&self.state.pool)
            .await;
        }
    }

    pub async fn process_project_spreadsheet(&self, id: Uuid, filename: String) {
        let _permit = self
            .state
            .semaphore
            .acquire()
            .await
            .expect("Semaphore closed");

        // Fetch record
        let upload_record =
            match sqlx::query!("SELECT term, job_id FROM project_uploads WHERE id = $1", id)
                .fetch_one(&self.state.pool)
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!("Failed to fetch project upload record: {}", e);
                    return;
                }
            };

        let _ = self
            .update_upload_status(id, DocumentStatus::Processing, None)
            .await;

        // Mark job as processing
        if let Some(job_id) = upload_record.job_id {
            let _ = sqlx::query!(
                "UPDATE jobs SET status = $1 WHERE id = $2 AND status = $3",
                JobStatus::Processing as JobStatus,
                job_id,
                JobStatus::Pending as JobStatus
            )
            .execute(&self.state.pool)
            .await;
        }

        let data = match self
            .state
            .storage
            .get_object("project-spreadsheets", &filename, None)
            .await
        {
            Ok(data) => data,
            Err(e) => {
                let err_msg = format!("Failed to download spreadsheet: {}", e);
                tracing::error!("{}", err_msg);
                let _ = self
                    .update_upload_status(id, DocumentStatus::Failed, Some(err_msg.clone()))
                    .await;
                if let Some(job_id) = upload_record.job_id {
                    self.record_job_error(job_id, id, err_msg).await;
                }
                return;
            }
        };

        let projects = if filename.ends_with(".csv") {
            Self::parse_csv(&data)
        } else if filename.ends_with(".xlsx") || filename.ends_with(".xls") {
            Self::parse_excel(&data)
        } else {
            let err_msg = format!("Unsupported file format: {}", filename);
            let _ = self
                .update_upload_status(id, DocumentStatus::Failed, Some(err_msg.clone()))
                .await;
            if let Some(job_id) = upload_record.job_id {
                self.record_job_error(job_id, id, err_msg).await;
            }
            return;
        };

        match projects {
            Ok(p) => {
                if let Err(e) = self.insert_projects(id, p, upload_record.term).await {
                    let err_msg = format!("Failed to insert projects into DB: {}", e);
                    tracing::error!("{}", err_msg);
                    let _ = self
                        .update_upload_status(id, DocumentStatus::Failed, Some(err_msg.clone()))
                        .await;
                    if let Some(job_id) = upload_record.job_id {
                        self.record_job_error(job_id, id, err_msg).await;
                    }
                } else {
                    let _ = self
                        .update_upload_status(id, DocumentStatus::Completed, None)
                        .await;
                }
            }
            Err(e) => {
                let err_msg = format!("Failed to parse spreadsheet: {}", e);
                tracing::error!("{}", err_msg);
                let _ = self
                    .update_upload_status(id, DocumentStatus::Failed, Some(err_msg.clone()))
                    .await;
                if let Some(job_id) = upload_record.job_id {
                    self.record_job_error(job_id, id, err_msg).await;
                }
            }
        }

        if let Some(job_id) = upload_record.job_id {
            self.maybe_mark_job_as_ready(job_id).await;
        }
    }

    pub fn parse_csv(data: &[u8]) -> anyhow::Result<Vec<ProjectData>> {
        let mut rdr = ReaderBuilder::new().has_headers(true).from_reader(data);

        // Normalize headers to lowercase
        let headers = rdr.headers()?.clone();
        let mut new_headers = csv::StringRecord::new();
        for h in headers.iter() {
            new_headers.push_field(&h.to_lowercase());
        }

        // Set the normalized headers back into the reader
        rdr.set_headers(new_headers);

        let mut projects = Vec::new();
        for result in rdr.deserialize() {
            let project: ProjectData = result?;
            projects.push(project);
        }
        Ok(projects)
    }

    pub fn parse_excel(data: &[u8]) -> anyhow::Result<Vec<ProjectData>> {
        let cursor = Cursor::new(data);
        let mut excel: Xlsx<_> = open_workbook_from_rs(cursor)?;

        let range = excel
            .worksheet_range_at(0)
            .ok_or_else(|| anyhow::anyhow!("No sheets found in Excel file"))??;

        let mut projects = Vec::new();
        let mut headers = Vec::new();

        for (i, row) in range.rows().enumerate() {
            if i == 0 {
                headers = row.iter().map(|c| c.to_string().to_lowercase()).collect();
                continue;
            }

            let mut title = String::new();
            let mut description = String::new();
            let mut requirements = String::new();
            let mut manager = String::new();
            let mut deadline = String::new();
            let mut priority = 0i16;
            let mut intern_cap = 1i16;

            for (j, cell) in row.iter().enumerate() {
                let header = headers.get(j).map(|s| s.as_str()).unwrap_or("");
                match header {
                    "title" | "project name" => title = cell.to_string(),
                    "description" | "about" => description = cell.to_string(),
                    "requirements" | "skills" => requirements = cell.to_string(),
                    "manager" | "lead" => manager = cell.to_string(),
                    "deadline" | "due date" => deadline = cell.to_string(),
                    "priority" => priority = cell.as_i64().unwrap_or(0) as i16,
                    "intern_cap" | "capacity" | "interns" => {
                        intern_cap = cell.as_i64().unwrap_or(1) as i16
                    }
                    _ => {}
                }
            }

            if !title.is_empty() {
                projects.push(ProjectData {
                    title,
                    description,
                    requirements,
                    manager,
                    deadline,
                    priority,
                    intern_cap,
                });
            }
        }
        Ok(projects)
    }

    async fn insert_projects(
        &self,
        upload_id: Uuid,
        projects: Vec<ProjectData>,
        term: Option<String>,
    ) -> Result<(), sqlx::Error> {
        if projects.is_empty() {
            return Ok(());
        }

        let mut query_builder: sqlx::QueryBuilder<sqlx::Postgres> = sqlx::QueryBuilder::new(
            "INSERT INTO projects (upload_id, title, description, requirements, manager, deadline, priority, intern_cap, term) ",
        );

        query_builder.push_values(projects, |mut b, p| {
            b.push_bind(upload_id)
                .push_bind(p.title)
                .push_bind(p.description)
                .push_bind(p.requirements)
                .push_bind(p.manager)
                .push_bind(p.deadline)
                .push_bind(p.priority)
                .push_bind(p.intern_cap)
                .push_bind(term.clone());
        });

        let query = query_builder.build();
        query.execute(&self.state.pool).await?;

        Ok(())
    }
}

#[derive(Debug, serde::Deserialize, PartialEq)]
pub struct ProjectData {
    #[serde(
        alias = "Project Name",
        alias = "project name",
        alias = "Title",
        alias = "title"
    )]
    pub title: String,
    #[serde(
        alias = "Description",
        alias = "description",
        alias = "About",
        alias = "about"
    )]
    pub description: String,
    #[serde(
        alias = "Requirements",
        alias = "requirements",
        alias = "Skills",
        alias = "skills"
    )]
    pub requirements: String,
    #[serde(alias = "Manager", alias = "manager", alias = "Lead", alias = "lead")]
    pub manager: String,
    #[serde(
        alias = "Deadline",
        alias = "deadline",
        alias = "Due Date",
        alias = "due date"
    )]
    pub deadline: String,
    #[serde(default)]
    pub priority: i16,
    #[serde(
        alias = "Capacity",
        alias = "capacity",
        alias = "Interns",
        alias = "interns",
        default = "default_cap"
    )]
    pub intern_cap: i16,
}

fn default_cap() -> i16 {
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_csv_valid() {
        let csv_data = b"title,description,requirements,manager,deadline,priority,intern_cap\nProject A,Desc A,Req A,Manager A,2026-01-01,1,2";
        let result = ProjectService::parse_csv(csv_data).unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(
            result[0],
            ProjectData {
                title: "Project A".to_string(),
                description: "Desc A".to_string(),
                requirements: "Req A".to_string(),
                manager: "Manager A".to_string(),
                deadline: "2026-01-01".to_string(),
                priority: 1,
                intern_cap: 2,
            }
        );
    }

    #[test]
    fn test_parse_csv_missing_optional_fields() {
        // Test with aliases and missing priority/cap
        let csv_data =
            b"Project Name,About,Skills,Lead,Due Date\nProject B,Desc B,Req B,Manager B,2026-02-02";
        let result = ProjectService::parse_csv(csv_data).unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].title, "Project B");
        assert_eq!(result[0].priority, 0); // Default
        assert_eq!(result[0].intern_cap, 1); // Default
    }

    #[test]
    fn test_parse_csv_missing_required_field() {
        let csv_data = b"description,requirements\nOnly desc,Only req";
        let result = ProjectService::parse_csv(csv_data);
        // Should fail because 'title' is a required field and not in the header
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_csv_header_aliases() {
        // Test with mix of different aliases and casing
        let csv_data = b"PROJECT NAME,about,SKILLS,Lead,due date,priority,interns\nAlias Project,About Alias,Req Alias,Lead Alias,2026-03-03,5,10";
        let result = ProjectService::parse_csv(csv_data).unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(
            result[0],
            ProjectData {
                title: "Alias Project".to_string(),
                description: "About Alias".to_string(),
                requirements: "Req Alias".to_string(),
                manager: "Lead Alias".to_string(),
                deadline: "2026-03-03".to_string(),
                priority: 5,
                intern_cap: 10,
            }
        );
    }

    #[test]
    fn test_parse_csv_empty() {
        let csv_data = b"";
        let result = ProjectService::parse_csv(csv_data).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_excel_valid() {
        // Read the sample Excel file from the workspace
        let path = "test-project-sheets/test_projects.xlsx";
        let excel_data = std::fs::read(path).expect("Failed to read test excel file");

        let result = ProjectService::parse_excel(&excel_data).unwrap();

        // We expect at least some projects based on the file name
        assert_eq!(result.len(), 4);

        // Verify the first project has expected values (matching test_projects.csv)
        assert_eq!(result[0].title, "Cloud Migration");
        assert_eq!(result[0].manager, "John Doe");
        assert_eq!(result[0].priority, 1);
        assert_eq!(result[0].intern_cap, 3);

        // Verify aliases are handled correctly (Interns -> intern_cap)
        assert_eq!(result[1].title, "Fleet Telematics UI");
        assert_eq!(result[1].intern_cap, 2);
    }
    #[test]
    fn test_parse_excel_invalid_data() {
        // Provide random non-excel bytes
        let excel_data = b"this is not an excel file";
        let result = ProjectService::parse_excel(excel_data);

        // Should return an error because it's not a valid .xlsx file
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_excel_broken() {
        let path = "test-project-sheets/test_projects_broken.xlsx";
        let excel_data = std::fs::read(path).expect("Failed to read test excel file");

        // This file is "broken" - it might have missing headers or other issues.
        // Our current implementation tries to extract what it can.
        let result = ProjectService::parse_excel(&excel_data);

        // Depending on how "broken" it is, it should either return Err or an empty Vec if no titles are found
        match result {
            Ok(projects) => {
                // If it succeeded, check if it actually found any valid projects (with titles)
                for p in projects {
                    assert!(!p.title.is_empty());
                }
            }
            Err(_) => {
                // Error is also an acceptable outcome for a truly corrupt file
            }
        }
    }
}
