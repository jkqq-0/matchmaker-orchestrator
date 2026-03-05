# Job Tracking Plan

## Part One: Resume Upload Table

The project upload system works like this. User puts project csv or xslx in the bucket, triggers insert on `public.project_uploads`, that insert triggers Rust API, which downloads the file, processes it, and puts the projects in `public.projects` and also sets the corresponding `public.project_uploads` row to Completed on the JobStatus.

So here's the idea. What if we did something like that for resumes? If an individual pdf gets uploaded, a row is inserted on `public.pdf_uploads`, which triggers the Rust API, which processes the resume and inserts it on `public.resumes.` 

The reason I want to do this is there's a use case where the user inserts an individual intern into the database. In this case, I don't want to trigger the Rust API because there's nothing for it to process.


## Part Two: Job Tracking Proper

### Objective
Provide a reliable, automated trigger for the Python matching script that ensures data is "Stable" and "Complete" for a specific term before processing begins.

### 1. Database Schema: Term-Based Orchestration
Instead of tracking individual files, we track the **readiness of a Term**.

#### New Enum: `matching_status`
- `idle`: Data is incomplete or ingestion is currently active.
- `ready`: Term is stable (all uploads terminal) and has sufficient data (>=1 project, >=1 resume).
- `processing`: Python matching script is currently running.
- `completed`: Matching finished for the current data state.
- `failed`: The matching script encountered a system error.

#### Updated `public.jobs` Table
```sql
CREATE TABLE public.jobs (
    term text PRIMARY KEY,
    status public.matching_status NOT NULL DEFAULT 'idle'::public.matching_status,
    updated_at timestamptz DEFAULT now(),
    last_error text
);
```

### 2. Readiness Logic: "Stable & Complete"
A term is considered **Ready** if:
1.  **Ingestion is Silent**: There are zero records in `resume_uploads`, `project_uploads`, or `zip_archives` for that term with a status of `pending` or `processing`.
2.  **Data is Present**: There is at least **one** record in `public.projects` AND at least **one** record in `public.resumes` for that term.

**Handling Failures**: If a file in a ZIP fails, it reaches the `failed` terminal state. Since it is no longer `pending` or `processing`, the term can still move to `ready` once all other files are finished.

### 3. Orchestration Mechanics (SQL Triggers)

#### The Refresh Function: `fn_refresh_matching_job(target_term)`
This function is called by triggers whenever data changes. It:
1.  Calculates the counts of active ingestions and available data.
2.  Updates `public.jobs` status to `ready` only if criteria are met.
3.  Executes `NOTIFY matching_job_ready, 'Term Name';` if the status transitions to `ready`.

#### Trigger Sources:
The refresh function is triggered by `INSERT`, `UPDATE`, or `DELETE` on:
- `public.resume_uploads` (Tracks individual and ZIP-extracted resumes)
- `public.project_uploads` (Tracks project spreadsheets)
- `public.zip_archives` (Tracks the parent ZIP process)
- `public.resumes` / `public.projects` (Tracks the actual data availability)

### 4. Python Worker Pattern: "Check + Listen"
To ensure the Python script "wakes up" reliably, it follows a hybrid pattern:

1.  **On Startup (Catch-up)**:
    - Query `SELECT term FROM jobs WHERE status = 'ready'`.
    - Process any terms that were prepared while the script was offline.
2.  **While Running (Real-time)**:
    - Establish a persistent connection and run `LISTEN matching_job_ready;`.
    - Wait (blocking) for notifications.
    - When a notification arrives, update status to `processing` and begin matching.

### 5. Implementation Roadmap
1.  **Migration**: Create the `matching_status` enum and `jobs` table.
2.  **Function**: Implement `fn_refresh_matching_job` with the "Stable & Complete" logic.
3.  **Triggers**: Attach the function to all relevant ingestion and data tables.
4.  **Python Update**: Implement the `psycopg2` Listen/Notify loop in the matching script.
