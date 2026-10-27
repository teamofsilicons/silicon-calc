-- A submitted import outlives its HTTP request and the browser. Source chunks are
-- content-addressed blobs, included in the normal database + blob backup.
create table import_jobs (
    id uuid primary key,
    owner uuid not null,
    caller_id text not null,
    caller_kind text not null,
    request jsonb not null,
    state text not null default 'uploading' check (state in ('uploading','queued','processing','completed','failed','cancelled')),
    result jsonb,
    error jsonb,
    dismissed boolean not null default false,
    attempts integer not null default 0,
    next_attempt_at timestamptz not null default now(),
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);
create index import_jobs_owner on import_jobs(owner, created_at);
create index import_jobs_pending on import_jobs(next_attempt_at, created_at) where state in ('queued','processing');
create table import_chunks (
    job_id uuid not null references import_jobs(id) on delete cascade,
    part integer not null check (part >= 0),
    bytes integer not null check (bytes > 0),
    blob text not null,
    primary key (job_id, part)
);
