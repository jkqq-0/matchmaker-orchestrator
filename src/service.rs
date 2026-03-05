use std::io::{Read, Write, Cursor};
use axum::body::Bytes;
use serde_json::{Value, from_str};
use tempfile::tempfile;
use uuid::Uuid;
use aws_sdk_s3::primitives::ByteStream;
use crate::AppState;
use crate::requests::openai::generate_structure_from_pdf;
use calamine::{Reader, Xlsx, open_workbook_from_rs, DataType};
use csv::ReaderBuilder;

#[derive(Debug, sqlx::Type)]
#[sqlx(type_name = "job_status", rename_all = "lowercase")]
pub enum JobStatus {
    Pending,
    Processing,
    Completed,
    Failed,
}

pub struct ResumeService {
    state: AppState,
}

impl ResumeService {
    pub fn new(state: AppState) -> Self {
        Self { state }
    }

    pub async fn process_and_update_resume(&self, id: Uuid, filename: String) {
        let _permit = self.state.semaphore.acquire().await.expect("Semaphore closed");
        
        // Mark as processing
        if let Err(e) = self.update_resume_status(id, JobStatus::Processing, None).await {
            tracing::error!("Failed to update status to Processing for resume {}: {}", id, e);
        }

        // Download
        let pdf_data = match self.state.s3_client.get_object()
            .bucket("resumes")
            .key(&filename)
            .send()
            .await {
                Ok(output) => {
                    match output.body.collect().await {
                        Ok(data) => data.into_bytes(),
                        Err(e) => {
                            let err_msg = format!("Failed to collect pdf body: {}", e);
                            tracing::error!("{}, filename {}, id {}", err_msg, filename, id);
                            if let Err(stat_e) = self.update_resume_status(id, JobStatus::Failed, Some(err_msg)).await {
                                tracing::error!("Failed to update status to Failed for resume {}: {}", id, stat_e);
                            }
                            return;
                        }
                    }
                }
                Err(e) => {
                    let err_msg = format!("Failed to download pdf: {:#?}", e);
                    tracing::error!("{}, filename {}, id {}", err_msg, filename, id);
                    if let Err(stat_e) = self.update_resume_status(id, JobStatus::Failed, Some(err_msg)).await {
                        tracing::error!("Failed to update status to Failed for resume {}: {}", id, stat_e);
                    }
                    return;
                }
            };

        // Parse and process
        match self.process_single_pdf(&pdf_data, &filename, id).await {
            Some((pdf_text, parsed_json)) => {
                match self.update_resume_record(id, pdf_text, parsed_json).await {
                    Ok(_) => {
                        tracing::info!("Resume record {} (filename: {}) updated successfully", id, filename);
                        if let Err(e) = self.update_resume_status(id, JobStatus::Completed, None).await {
                            tracing::error!("Failed to update status to Completed for resume {}: {}", id, e);
                        }
                    }
                    Err(e) => {
                        let err_msg = format!("Failed to update database record: {}", e);
                        tracing::error!("{}, filename {}, id {}", err_msg, filename, id);
                        if let Err(stat_e) = self.update_resume_status(id, JobStatus::Failed, Some(err_msg)).await {
                            tracing::error!("Failed to update status to Failed for resume {}: {}", id, stat_e);
                        }
                    }
                }
            }
            None => {
                 let err_msg = "PDF processing or LLM parsing failed".to_string();
                 tracing::warn!("{}, filename {}, id {}", err_msg, filename, id);
                 if let Err(e) = self.update_resume_status(id, JobStatus::Failed, Some(err_msg)).await {
                    tracing::error!("Failed to update status to Failed for resume {}: {}", id, e);
                 }
            }
        }
    }

    async fn update_resume_status(&self, id: Uuid, status: JobStatus, error_message: Option<String>) -> Result<(), sqlx::Error> {
        sqlx::query!(
            "UPDATE resumes SET status = $1, error_message = $2 WHERE id = $3",
            status as JobStatus,
            error_message,
            id
        )
        .execute(&self.state.pool)
        .await?;
        Ok(())
    }

    pub async fn process_single_pdf(
        &self,
        pdf_data: &Bytes,
        filename: &str,
        id: Uuid,
    ) -> Option<(String, Value)> {
        let pdf_text = match pdf_extract::extract_text_from_mem(pdf_data) {
            Ok(text) => text,
            Err(e) => {
                tracing::error!("Failed to extract text from PDF for filename {}, id {}: {}", filename, id, e);
                return None;
            }
        };
        
        // Removed PII logging of full PDF text
        
        let response = generate_structure_from_pdf(
            &pdf_text, 
            &self.state.http_client, 
            &self.state.openai_api_key, 
            &self.state.resume_schema
        ).await;

        match response {
            Ok(r) => {
                if let Some(choice) = r.choices.first() {
                    let raw_content = &choice.message.content;
                    match from_str::<Value>(raw_content) {
                        Ok(parsed_json) => {
                             tracing::info!("LLM-generated JSON received for filename {}, id {}", filename, id);
                             // Removed PII logging of parsed JSON
                             Some((pdf_text, parsed_json))
                        },
                        Err(e) => {
                            tracing::error!("LLM returned invalid JSON for filename {}, id {}: {}", filename, id, e);
                            tracing::debug!("Raw content: {}", raw_content);
                            None
                        }
                    }
                }
                else {
                    tracing::error!("No choices returned from LLM for filename {}, id {}", filename, id);
                    None
                }
            }
            Err(e) => {
                tracing::error!("LLM request failed for filename {}, id {}: {:#?}", filename, id, e);
                None
            }
        }
    }

    pub async fn update_resume_record(&self, id: Uuid, text: String, structured_json : Value) -> Result<sqlx::postgres::PgQueryResult, sqlx::Error> {
        sqlx::query!("UPDATE resumes SET text = $1, structured = $2 WHERE id = $3", text, structured_json, id)
            .execute(&self.state.pool)
            .await
    }

    async fn update_zip_status(&self, id: Uuid, status: JobStatus, error_message: Option<String>) -> Result<(), sqlx::Error> {
        sqlx::query!(
            "UPDATE zip_archives SET status = $1, error_message = $2 WHERE id = $3",
            status as JobStatus,
            error_message,
            id
        )
        .execute(&self.state.pool)
        .await?;
        Ok(())
    }

    pub async fn handle_batch_extraction(&self, id: Uuid, filename: String) {
        let _permit = self.state.semaphore.acquire().await.expect("Semaphore closed");

        // Mark as processing
        if let Err(e) = self.update_zip_status(id, JobStatus::Processing, None).await {
            tracing::error!("Failed to update status to Processing for zip {}: {}", id, e);
        }

        let zip_data = match self.state.s3_client.get_object()
            .bucket("zip-archives")
            .key(&filename)
            .send()
            .await {
                Ok(output) => {
                    match output.body.collect().await {
                        Ok(data) => data.into_bytes(),
                        Err(e) => {
                            let err_msg = format!("Failed to collect zip body: {}", e);
                            tracing::error!("{}, filename {}, id {}", err_msg, filename, id);
                            if let Err(stat_e) = self.update_zip_status(id, JobStatus::Failed, Some(err_msg)).await {
                                tracing::error!("Failed to update status to Failed for zip {}: {}", id, stat_e);
                            }
                            return;
                        }
                    }
                }
                Err(e) => {
                    let err_msg = format!("Failed to download zip: {:#?}", e);
                    tracing::error!("{}, filename {}, id {}", err_msg, filename, id);
                    if let Err(stat_e) = self.update_zip_status(id, JobStatus::Failed, Some(err_msg)).await {
                        tracing::error!("Failed to update status to Failed for zip {}: {}", id, stat_e);
                    }
                    return;
                }
            };
            
        let mut tmp_file = match tempfile() {
             Ok(f) => f,
             Err(e) => {
                 let err_msg = format!("Failed to create tempfile: {}", e);
                 tracing::error!("{}", err_msg);
                 if let Err(stat_e) = self.update_zip_status(id, JobStatus::Failed, Some(err_msg)).await {
                     tracing::error!("Failed to update status to Failed for zip {}: {}", id, stat_e);
                 }
                 return;
             }
        };
        
        if let Err(e) = tmp_file.write_all(&zip_data) {
             let err_msg = format!("Failed to write to tempfile: {}", e);
             tracing::error!("{}", err_msg);
             if let Err(stat_e) = self.update_zip_status(id, JobStatus::Failed, Some(err_msg)).await {
                 tracing::error!("Failed to update status to Failed for zip {}: {}", id, stat_e);
             }
             return;
        }
        
        let mut archive = match zip::ZipArchive::new(tmp_file) {
            Ok(a) => a,
            Err(e) => {
                let err_msg = format!("Failed to create zip archive: {}", e);
                tracing::error!("{}", err_msg);
                if let Err(stat_e) = self.update_zip_status(id, JobStatus::Failed, Some(err_msg)).await {
                    tracing::error!("Failed to update status to Failed for zip {}: {}", id, stat_e);
                }
                return;
            }
        };
        
        tracing::info!("Successfully extracted zip archive with {} files", archive.len());
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

            let mut pdf_buffer = Vec::new();
            if let Err(e) = file.read_to_end(&mut pdf_buffer) {
                tracing::error!("Failed to read file {} to buffer: {}", file.name(), e);
                continue;
            }
            
            let pdf_bytes = Bytes::from(pdf_buffer);
            let pdf_name = file.name().to_string();
            let upload_path = format!("{}_{}", filename, pdf_name);
            
            let s3_client = self.state.s3_client.clone();
            let semaphore = self.state.semaphore.clone();
            let zip_id_str = id.to_string();

            tokio::spawn(async move {
                let _permit = semaphore.acquire().await.expect("Semaphore closed");

                match s3_client.put_object()
                    .bucket("resumes")
                    .key(&upload_path)
                    .body(ByteStream::from(pdf_bytes))
                    .content_type("application/pdf")
                    .metadata("zip_id", zip_id_str)
                    .send()
                    .await {
                        Ok(_) => {
                             tracing::info!("Successfully re-uploaded extracted PDF: {}", upload_path);
                             // Linkage now handled by the storage trigger on objects
                        },
                        Err(e) => tracing::error!("Failed to upload extracted PDF {}: {}", upload_path, e),
                    }
            });
        }
        
        if let Err(e) = self.update_zip_status(id, JobStatus::Completed, None).await {
            tracing::error!("Failed to update status to Completed for zip {}: {}", id, e);
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

    async fn update_upload_status(&self, id: Uuid, status: JobStatus, error_message: Option<String>) -> Result<(), sqlx::Error> {
        sqlx::query!(
            "UPDATE project_uploads SET status = $1, error_message = $2 WHERE id = $3",
            status as JobStatus,
            error_message,
            id
        )
        .execute(&self.state.pool)
        .await?;
        Ok(())
    }

    pub async fn process_project_spreadsheet(&self, id: Uuid, filename: String) {
        let _permit = self.state.semaphore.acquire().await.expect("Semaphore closed");
        
        let _ = self.update_upload_status(id, JobStatus::Processing, None).await;

        let data = match self.state.s3_client.get_object()
            .bucket("project-spreadsheets")
            .key(&filename)
            .send()
            .await {
                Ok(output) => {
                    match output.body.collect().await {
                        Ok(d) => d.into_bytes(),
                        Err(e) => {
                            let err_msg = format!("Failed to collect spreadsheet body: {}", e);
                            tracing::error!("{}", err_msg);
                            let _ = self.update_upload_status(id, JobStatus::Failed, Some(err_msg)).await;
                            return;
                        }
                    }
                }
                Err(e) => {
                    let err_msg = format!("Failed to download spreadsheet: {:#?}", e);
                    tracing::error!("{}", err_msg);
                    let _ = self.update_upload_status(id, JobStatus::Failed, Some(err_msg)).await;
                    return;
                }
            };

        let projects = if filename.ends_with(".csv") {
            Self::parse_csv(&data)
        } else if filename.ends_with(".xlsx") || filename.ends_with(".xls") {
            Self::parse_excel(&data)
        } else {
            let err_msg = format!("Unsupported file format: {}", filename);
            let _ = self.update_upload_status(id, JobStatus::Failed, Some(err_msg)).await;
            return;
        };

        match projects {
            Ok(p) => {
                if let Err(e) = self.insert_projects(id, p).await {
                    let err_msg = format!("Failed to insert projects into DB: {}", e);
                    tracing::error!("{}", err_msg);
                    let _ = self.update_upload_status(id, JobStatus::Failed, Some(err_msg)).await;
                } else {
                    let _ = self.update_upload_status(id, JobStatus::Completed, None).await;
                }
            }
            Err(e) => {
                let err_msg = format!("Failed to parse spreadsheet: {}", e);
                tracing::error!("{}", err_msg);
                let _ = self.update_upload_status(id, JobStatus::Failed, Some(err_msg)).await;
            }
        }
    }

    pub fn parse_csv(data: &[u8]) -> anyhow::Result<Vec<ProjectData>> {
        let mut rdr = ReaderBuilder::new()
            .has_headers(true)
            .from_reader(data);
        
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
        
        let range = excel.worksheet_range_at(0)
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
                    "intern_cap" | "capacity" | "interns" => intern_cap = cell.as_i64().unwrap_or(1) as i16,
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

    async fn insert_projects(&self, upload_id: Uuid, projects: Vec<ProjectData>) -> Result<(), sqlx::Error> {
        // Fetch the term from the project_uploads table
        let term = sqlx::query!(
            "SELECT term FROM project_uploads WHERE id = $1",
            upload_id
        )
        .fetch_one(&self.state.pool)
        .await?
        .term;

        for p in projects {
            sqlx::query!(
                "INSERT INTO projects (upload_id, title, description, requirements, manager, deadline, priority, intern_cap, term)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
                upload_id,
                p.title,
                p.description,
                p.requirements,
                p.manager,
                p.deadline,
                p.priority,
                p.intern_cap,
                term
            )
            .execute(&self.state.pool)
            .await?;
        }
        Ok(())
    }
}

#[derive(Debug, serde::Deserialize, PartialEq)]
pub struct ProjectData {
    #[serde(alias = "Project Name", alias = "project name", alias = "Title", alias = "title")]
    pub title: String,
    #[serde(alias = "Description", alias = "description", alias = "About", alias = "about")]
    pub description: String,
    #[serde(alias = "Requirements", alias = "requirements", alias = "Skills", alias = "skills")]
    pub requirements: String,
    #[serde(alias = "Manager", alias = "manager", alias = "Lead", alias = "lead")]
    pub manager: String,
    #[serde(alias = "Deadline", alias = "deadline", alias = "Due Date", alias = "due date")]
    pub deadline: String,
    #[serde(default)]
    pub priority: i16,
    #[serde(alias = "Capacity", alias = "capacity", alias = "Interns", alias = "interns", default = "default_cap")]
    pub intern_cap: i16,
}

fn default_cap() -> i16 { 1 }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_csv_valid() {
        let csv_data = b"title,description,requirements,manager,deadline,priority,intern_cap\nProject A,Desc A,Req A,Manager A,2026-01-01,1,2";
        let result = ProjectService::parse_csv(csv_data).unwrap();
        
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], ProjectData {
            title: "Project A".to_string(),
            description: "Desc A".to_string(),
            requirements: "Req A".to_string(),
            manager: "Manager A".to_string(),
            deadline: "2026-01-01".to_string(),
            priority: 1,
            intern_cap: 2,
        });
    }

    #[test]
    fn test_parse_csv_missing_optional_fields() {
        // Test with aliases and missing priority/cap
        let csv_data = b"Project Name,About,Skills,Lead,Due Date\nProject B,Desc B,Req B,Manager B,2026-02-02";
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
        assert!(!result.is_empty());
        
        // Verify the first project has a title
        assert!(!result[0].title.is_empty());
        tracing::debug!("Parsed {} projects from excel", result.len());
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
            },
            Err(_) => {
                // Error is also an acceptable outcome for a truly corrupt file
            }
        }
    }
}


