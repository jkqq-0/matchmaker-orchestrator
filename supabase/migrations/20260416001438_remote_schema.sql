drop trigger if exists "orchestrator-ingest-projects" on "public"."project_uploads";

drop trigger if exists "orchestrator-ingest-interns-individual" on "public"."resume_uploads";

drop trigger if exists "orchestrator-injest-resumes-batch" on "public"."zip_archives";

drop policy "Enable read access authenticated usesrs" on "public"."results";

revoke delete on table "public"."matches" from "anon";

revoke insert on table "public"."matches" from "anon";

revoke references on table "public"."matches" from "anon";

revoke select on table "public"."matches" from "anon";

revoke trigger on table "public"."matches" from "anon";

revoke truncate on table "public"."matches" from "anon";

revoke update on table "public"."matches" from "anon";

revoke delete on table "public"."matches" from "authenticated";

revoke insert on table "public"."matches" from "authenticated";

revoke references on table "public"."matches" from "authenticated";

revoke select on table "public"."matches" from "authenticated";

revoke trigger on table "public"."matches" from "authenticated";

revoke truncate on table "public"."matches" from "authenticated";

revoke update on table "public"."matches" from "authenticated";

revoke delete on table "public"."matches" from "service_role";

revoke insert on table "public"."matches" from "service_role";

revoke references on table "public"."matches" from "service_role";

revoke select on table "public"."matches" from "service_role";

revoke trigger on table "public"."matches" from "service_role";

revoke truncate on table "public"."matches" from "service_role";

revoke update on table "public"."matches" from "service_role";

revoke delete on table "public"."results" from "anon";

revoke insert on table "public"."results" from "anon";

revoke references on table "public"."results" from "anon";

revoke select on table "public"."results" from "anon";

revoke trigger on table "public"."results" from "anon";

revoke truncate on table "public"."results" from "anon";

revoke update on table "public"."results" from "anon";

revoke delete on table "public"."results" from "authenticated";

revoke insert on table "public"."results" from "authenticated";

revoke references on table "public"."results" from "authenticated";

revoke select on table "public"."results" from "authenticated";

revoke trigger on table "public"."results" from "authenticated";

revoke truncate on table "public"."results" from "authenticated";

revoke update on table "public"."results" from "authenticated";

revoke delete on table "public"."results" from "service_role";

revoke insert on table "public"."results" from "service_role";

revoke references on table "public"."results" from "service_role";

revoke select on table "public"."results" from "service_role";

revoke trigger on table "public"."results" from "service_role";

revoke truncate on table "public"."results" from "service_role";

revoke update on table "public"."results" from "service_role";

alter table "public"."matches" drop constraint "matches_intern_id_fkey";

alter table "public"."matches" drop constraint "matches_project_id_fkey";

alter table "public"."matches" drop constraint "matches_pkey";

drop index if exists "public"."matches_pkey";

drop table "public"."matches";

drop table "public"."results";

set check_function_bodies = off;

CREATE OR REPLACE FUNCTION public.handle_new_project_upload()
 RETURNS trigger
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'public'
AS $function$
DECLARE
  job_id_val uuid;
BEGIN
  IF new.bucket_id = 'project-spreadsheets' AND new.name NOT LIKE '%.emptyFolderPlaceholder' THEN
    BEGIN
      job_id_val := (new.user_metadata ->> 'job_id')::uuid;
      IF job_id_val IS NULL THEN
        job_id_val := (new.metadata ->> 'job_id')::uuid;
      END IF;
    EXCEPTION WHEN others THEN
      job_id_val := NULL;
    END;

    INSERT INTO public.project_uploads (filename, user_id, term, job_id)
    VALUES (
      new.name,
      new.owner,
      CASE WHEN position('/' in new.name) > 0 THEN split_part(new.name, '/', 1) ELSE NULL END,
      job_id_val
    );
  END IF;
  RETURN new;
END;
$function$
;

CREATE OR REPLACE FUNCTION public.handle_new_zip_upload()
 RETURNS trigger
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'public'
AS $function$
declare
  job_id_val uuid;
BEGIN
  IF new.bucket_id = 'zip-archives' AND new.name NOT LIKE '%.emptyFolderPlaceholder' THEN
    BEGIN
      job_id_val := (new.user_metadata ->> 'job_id')::uuid;
      IF job_id_val IS NULL THEN
        job_id_val := (new.metadata ->> 'job_id')::uuid;
      END IF;
    EXCEPTION WHEN others THEN
      job_id_val := NULL;
    END;

    INSERT INTO public.zip_archives (filename, user_id, term, job_id)
    VALUES (
      new.name,
      new.owner,
      CASE WHEN position('/' in new.name) > 0 THEN split_part(new.name, '/', 1) ELSE NULL END,
      job_id_val
    );
  END IF;
  RETURN new;
END;
$function$
;

CREATE OR REPLACE FUNCTION public.notify_orchestrator_secure()
 RETURNS trigger
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'public', 'extensions', 'vault', 'net'
AS $function$
DECLARE
  secret text;
  token text;
  url text := TG_ARGV[0];
  headers jsonb;
  request_id bigint;
  payload jsonb;
  timeout_ms integer := 5000;
BEGIN
  -- Get Secret
  SELECT decrypted_secret INTO secret
  FROM vault.decrypted_secrets
  WHERE name = 'app_jwt_secret';

  IF secret IS NULL THEN
    RAISE EXCEPTION 'app_jwt_secret not found in vault';
  END IF;

  -- Sign Token (extensions.sign takes json, not jsonb)
  token := extensions.sign(
    json_build_object(
      'role', 'service_role',
      'iss', 'supabase',
      'exp', (extract(epoch from now()) + 3600)::bigint,
      'sub', 'orchestrator'
    ),
    secret
  );

  -- Build Headers
  headers := jsonb_build_object(
    'Content-Type', 'application/json',
    'Authorization', 'Bearer ' || token
  );

  -- Build Payload
  payload := jsonb_build_object(
    'old_record', to_jsonb(OLD),
    'record', to_jsonb(NEW),
    'type', TG_OP,
    'table', TG_TABLE_NAME,
    'schema', TG_TABLE_SCHEMA
  );

  -- Send Request (pg_net)
  SELECT net.http_post(
    url,
    payload,
    '{}'::jsonb,
    headers,
    timeout_ms
  ) INTO request_id;

  RETURN NEW;
END;
$function$
;


  create policy "Allow authenticated users to delete any project"
  on "public"."projects"
  as permissive
  for delete
  to public
using ((auth.role() = 'authenticated'::text));



  create policy "Allow authenticated users to update any project"
  on "public"."projects"
  as permissive
  for update
  to public
using ((auth.role() = 'authenticated'::text));



  create policy "Allow authenticated users to delete any resume"
  on "public"."resumes"
  as permissive
  for delete
  to public
using ((auth.role() = 'authenticated'::text));



  create policy "Allow authenticated users to update any resume"
  on "public"."resumes"
  as permissive
  for update
  to public
using ((auth.role() = 'authenticated'::text));


CREATE TRIGGER "TriggerPythonWebhook" AFTER INSERT OR UPDATE ON public.jobs FOR EACH ROW EXECUTE FUNCTION supabase_functions.http_request('https://mlm-odule-ipmm.vercel.app/webhook', 'POST', '{"Content-type":"application/json"}', '{}', '5000');

CREATE TRIGGER "orchestrator-ingest-projects" AFTER INSERT ON public.project_uploads FOR EACH ROW EXECUTE FUNCTION public.notify_orchestrator_secure('http://host.docker.internal:3000/ingest/projects');

CREATE TRIGGER "orchestrator-ingest-interns-individual" AFTER INSERT ON public.resume_uploads FOR EACH ROW EXECUTE FUNCTION public.notify_orchestrator_secure('http://host.docker.internal:3000/ingest/interns/individual');

CREATE TRIGGER "orchestrator-injest-resumes-batch" AFTER INSERT ON public.zip_archives FOR EACH ROW EXECUTE FUNCTION public.notify_orchestrator_secure('http://host.docker.internal:3000/ingest/interns/batch');

drop policy "Enable insert for authenticated users only" on "storage"."buckets";

drop policy "Enable insert for authenticated users only" on "storage"."objects";


  create policy "preprojectuploads auth insert"
  on "storage"."objects"
  as permissive
  for insert
  to authenticated
with check ((bucket_id = 'preprojectuploads'::text));



  create policy "preprojectuploads auth select"
  on "storage"."objects"
  as permissive
  for select
  to authenticated
using ((bucket_id = 'preprojectuploads'::text));



  create policy "preprojectuploads auth update"
  on "storage"."objects"
  as permissive
  for update
  to authenticated
using ((bucket_id = 'preprojectuploads'::text));



  create policy "preresumeuploads auth insert"
  on "storage"."objects"
  as permissive
  for insert
  to authenticated
with check ((bucket_id = 'preresumeuploads'::text));



  create policy "preresumeuploads auth select"
  on "storage"."objects"
  as permissive
  for select
  to authenticated
using ((bucket_id = 'preresumeuploads'::text));



  create policy "preresumeuploads auth update"
  on "storage"."objects"
  as permissive
  for update
  to authenticated
using ((bucket_id = 'preresumeuploads'::text));



  create policy "project-spreadsheets anon insert"
  on "storage"."objects"
  as permissive
  for insert
  to anon
with check ((bucket_id = 'project-spreadsheets'::text));



  create policy "project-spreadsheets auth insert"
  on "storage"."objects"
  as permissive
  for insert
  to authenticated
with check ((bucket_id = 'project-spreadsheets'::text));



  create policy "project-spreadsheets auth select"
  on "storage"."objects"
  as permissive
  for select
  to authenticated
using (((bucket_id = 'project-spreadsheets'::text) AND (auth.uid() IS NOT NULL)));



  create policy "project-spreadsheets auth update"
  on "storage"."objects"
  as permissive
  for update
  to authenticated
using ((bucket_id = 'project-spreadsheets'::text));



  create policy "resumes auth insert"
  on "storage"."objects"
  as permissive
  for insert
  to authenticated
with check ((bucket_id = 'resumes'::text));



  create policy "resumes auth select"
  on "storage"."objects"
  as permissive
  for select
  to authenticated
using ((bucket_id = 'resumes'::text));



  create policy "resumes auth update"
  on "storage"."objects"
  as permissive
  for update
  to authenticated
using ((bucket_id = 'resumes'::text));



  create policy "zip-archives anon insert"
  on "storage"."objects"
  as permissive
  for insert
  to anon
with check ((bucket_id = 'zip-archives'::text));



  create policy "zip-archives auth insert"
  on "storage"."objects"
  as permissive
  for insert
  to authenticated
with check ((bucket_id = 'zip-archives'::text));



  create policy "zip-archives auth select"
  on "storage"."objects"
  as permissive
  for select
  to authenticated
using (((bucket_id = 'zip-archives'::text) AND (auth.uid() IS NOT NULL)));



  create policy "zip-archives auth update"
  on "storage"."objects"
  as permissive
  for update
  to authenticated
using ((bucket_id = 'zip-archives'::text));



