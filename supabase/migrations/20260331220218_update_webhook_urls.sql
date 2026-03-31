-- 1. Projects Trigger
DROP TRIGGER IF EXISTS "orchestrator-ingest-projects" ON public.project_uploads;
CREATE TRIGGER "orchestrator-ingest-projects" 
AFTER INSERT ON public.project_uploads 
FOR EACH ROW EXECUTE FUNCTION public.notify_orchestrator_secure('https://api.demo.jbu-matchmaker-capstone.com/ingest/projects');

-- 2. Individual Resumes Trigger
DROP TRIGGER IF EXISTS "orchestrator-ingest-interns-individual" ON public.resume_uploads;
CREATE TRIGGER "orchestrator-ingest-interns-individual" 
AFTER INSERT ON public.resume_uploads 
FOR EACH ROW EXECUTE FUNCTION public.notify_orchestrator_secure('https://api.demo.jbu-matchmaker-capstone.com/ingest/interns/individual');

-- 3. Batch ZIP Resumes Trigger
DROP TRIGGER IF EXISTS "orchestrator-injest-resumes-batch" ON public.zip_archives;
CREATE TRIGGER "orchestrator-injest-resumes-batch" 
AFTER INSERT ON public.zip_archives 
FOR EACH ROW EXECUTE FUNCTION public.notify_orchestrator_secure('https://api.demo.jbu-matchmaker-capstone.com/ingest/interns/batch');
