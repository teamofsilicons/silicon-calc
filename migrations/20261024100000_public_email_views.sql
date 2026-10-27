alter table sheets add column require_email boolean not null default false;
alter table sheets add column email_gate_version bigint not null default 0;

-- Entered email addresses are claims, never account credentials or memberships.
create table sheet_viewers (
    sheet_id text not null references sheets(id) on delete cascade,
    viewer_key text not null,
    email text not null,
    account_uuid uuid,
    email_source text not null check (email_source in ('account', 'entered')),
    first_viewed_at timestamptz not null default now(),
    last_viewed_at timestamptz not null default now(),
    primary key (sheet_id, viewer_key)
);
create index sheet_viewers_recent on sheet_viewers(sheet_id, last_viewed_at desc, viewer_key);
create table sheet_view_grants (
    token_hash text primary key,
    sheet_id text not null references sheets(id) on delete cascade,
    email text not null,
    gate_version bigint not null,
    expires_at timestamptz not null default now() + interval '30 days'
);
create index sheet_view_grants_sheet on sheet_view_grants(sheet_id);
