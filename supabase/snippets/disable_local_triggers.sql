-- ===========================================================================
-- disable_local_triggers.sql
--
-- Run this against your LOCAL Supabase instance after `supabase start`.
-- It DISABLES the three webhook triggers that normally fire to your ngrok
-- endpoint. For load testing, the orchestrator is driven directly via HTTP
-- requests, so these triggers would cause duplicate processing.
--
-- Usage:
--   psql postgresql://postgres:postgres@127.0.0.1:54322/postgres \
--     -f scripts/disable_local_triggers.sql
--
-- DO NOT apply this to the cloud project.
-- ===========================================================================

-- Resume individual uploads trigger
DROP TRIGGER IF EXISTS "orchestrator-ingest-interns-individual" ON public.resume_uploads;

-- Batch (ZIP) uploads trigger
DROP TRIGGER IF EXISTS "orchestrator-injest-resumes-batch" ON public.zip_archives;

-- Project spreadsheet uploads trigger
DROP TRIGGER IF EXISTS "orchestrator-ingest-projects" ON public.project_uploads;

-- Confirm
SELECT
    trigger_name,
    event_object_table
FROM information_schema.triggers
WHERE trigger_schema = 'public'
  AND trigger_name IN (
    'orchestrator-ingest-interns-individual',
    'orchestrator-injest-resumes-batch',
    'orchestrator-ingest-projects'
  );
-- Should return 0 rows if all three triggers were dropped successfully.
