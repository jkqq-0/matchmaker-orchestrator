-- CI Schema Setup

CREATE TYPE document_status AS ENUM ('pending', 'processing', 'completed', 'failed');
CREATE TYPE job_status AS ENUM ('pending', 'processing', 'ready', 'matching', 'completed', 'failed');
CREATE TYPE match_decision AS ENUM ('rejected', 'accepted', 'pending');

CREATE TABLE project_uploads (
    id uuid PRIMARY KEY,
    filename text NOT NULL,
    status document_status DEFAULT 'pending',
    error_message text,
    user_id uuid,
    job_id uuid,
    term text,
    created_at timestamp with time zone DEFAULT now()
);

CREATE TABLE projects (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    upload_id uuid REFERENCES project_uploads(id),
    title text NOT NULL,
    description text,
    requirements text,
    manager text,
    deadline text,
    priority smallint DEFAULT 0,
    intern_cap smallint DEFAULT 1,
    term text,
    deliverable text,
    created_at timestamp with time zone DEFAULT now()
);

CREATE TABLE zip_archives (
    id uuid PRIMARY KEY,
    filename text NOT NULL,
    status document_status DEFAULT 'pending',
    error_message text,
    user_id uuid,
    job_id uuid,
    term text,
    expected_files integer DEFAULT 0,
    created_at timestamp with time zone DEFAULT now()
);

CREATE TABLE resume_uploads (
    id uuid PRIMARY KEY,
    filename text NOT NULL,
    status document_status DEFAULT 'pending',
    error_message text,
    user_id uuid,
    job_id uuid,
    zip_id uuid REFERENCES zip_archives(id),
    term text,
    created_at timestamp with time zone DEFAULT now()
);

CREATE TABLE resumes (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    upload_id uuid REFERENCES resume_uploads(id),
    zip_id uuid REFERENCES zip_archives(id),
    filename text NOT NULL,
    text text,
    structured json,
    user_id uuid,
    term text,
    created_at timestamp with time zone DEFAULT now()
);

CREATE TABLE jobs (
    id uuid PRIMARY KEY,
    term text,
    status job_status DEFAULT 'pending',
    rust_error jsonb DEFAULT '{"resumes": [], "projects": []}'::jsonb,
    python_error text,
    created_at timestamp with time zone DEFAULT now()
);
