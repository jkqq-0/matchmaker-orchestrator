-- =============================================================================
-- Storage Seed: Recreates cloud storage buckets and RLS policies locally.
-- Run automatically by `supabase db reset` or `supabase start`.
-- Last synced from cloud project: pkckwgszwgrvxwwdofcj
-- =============================================================================

-- -----------------------------------------------------------------------------
-- VAULT SECRETS (required by notify_orchestrator_secure trigger)
-- -----------------------------------------------------------------------------
-- The trigger that fires on resume_uploads / zip_archives / project_uploads
-- inserts will RAISE EXCEPTION if this secret is missing (P0001).
-- Replace the value with your actual JWT secret if you need real webhook calls,
-- or leave as-is for local dev (webhooks will fire but fail to reach localhost).

DO $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM vault.secrets WHERE name = 'app_jwt_secret') THEN
    PERFORM vault.create_secret(
      'super-secret-jwt-token-with-at-least-32-characters-long',
      'app_jwt_secret'
    );
  END IF;
END;
$$;

-- -----------------------------------------------------------------------------
-- BUCKETS
-- -----------------------------------------------------------------------------

INSERT INTO storage.buckets (id, name, public, file_size_limit, allowed_mime_types)
VALUES
  (
    'resumes',
    'resumes',
    false,
    null,
    null
  ),
  (
    'zip-archives',
    'zip-archives',
    false,
    null,
    ARRAY['application/zip', 'application/x-zip-compressed', 'application/x-zip']
  ),
  (
    'project-spreadsheets',
    'project-spreadsheets',
    false,
    null,
    ARRAY[
      'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet',
      'text/csv',
      'application/vnd.ms-excel'
    ]
  )
ON CONFLICT (id) DO NOTHING;

-- -----------------------------------------------------------------------------
-- RLS POLICIES — storage.objects
-- -----------------------------------------------------------------------------

-- resumes
DROP POLICY IF EXISTS "resumes auth insert" ON storage.objects;
CREATE POLICY "resumes auth insert"
  ON storage.objects FOR INSERT TO authenticated
  WITH CHECK (bucket_id = 'resumes');

DROP POLICY IF EXISTS "resumes auth select" ON storage.objects;
CREATE POLICY "resumes auth select"
  ON storage.objects FOR SELECT TO authenticated
  USING (bucket_id = 'resumes');

DROP POLICY IF EXISTS "resumes auth update" ON storage.objects;
CREATE POLICY "resumes auth update"
  ON storage.objects FOR UPDATE TO authenticated
  USING (bucket_id = 'resumes');

-- zip-archives
DROP POLICY IF EXISTS "zip-archives anon insert" ON storage.objects;
CREATE POLICY "zip-archives anon insert"
  ON storage.objects FOR INSERT TO anon
  WITH CHECK (bucket_id = 'zip-archives');

DROP POLICY IF EXISTS "zip-archives auth insert" ON storage.objects;
CREATE POLICY "zip-archives auth insert"
  ON storage.objects FOR INSERT TO authenticated
  WITH CHECK (bucket_id = 'zip-archives');

DROP POLICY IF EXISTS "zip-archives auth select" ON storage.objects;
CREATE POLICY "zip-archives auth select"
  ON storage.objects FOR SELECT TO authenticated
  USING ((bucket_id = 'zip-archives') AND (auth.uid() IS NOT NULL));

DROP POLICY IF EXISTS "zip-archives auth update" ON storage.objects;
CREATE POLICY "zip-archives auth update"
  ON storage.objects FOR UPDATE TO authenticated
  USING (bucket_id = 'zip-archives');

-- project-spreadsheets
DROP POLICY IF EXISTS "project-spreadsheets anon insert" ON storage.objects;
CREATE POLICY "project-spreadsheets anon insert"
  ON storage.objects FOR INSERT TO anon
  WITH CHECK (bucket_id = 'project-spreadsheets');

DROP POLICY IF EXISTS "project-spreadsheets auth insert" ON storage.objects;
CREATE POLICY "project-spreadsheets auth insert"
  ON storage.objects FOR INSERT TO authenticated
  WITH CHECK (bucket_id = 'project-spreadsheets');

DROP POLICY IF EXISTS "project-spreadsheets auth select" ON storage.objects;
CREATE POLICY "project-spreadsheets auth select"
  ON storage.objects FOR SELECT TO authenticated
  USING ((bucket_id = 'project-spreadsheets') AND (auth.uid() IS NOT NULL));

DROP POLICY IF EXISTS "project-spreadsheets auth update" ON storage.objects;
CREATE POLICY "project-spreadsheets auth update"
  ON storage.objects FOR UPDATE TO authenticated
  USING (bucket_id = 'project-spreadsheets');