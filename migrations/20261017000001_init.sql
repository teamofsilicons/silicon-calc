-- Silicon Calc: everything except cell contents, which live in operations and snapshots (blob storage).
-- Accounts are stored by their Silicon Accounts uuid only; ids and names are looked up when shown.

create extension if not exists citext;

-- Accounts Calc has seen sign in, with what Calc may show and the email used to match invites.
create table known_accounts (
    uuid uuid primary key,
    public_id text not null,
    kind text not null check (kind in ('carbon', 'silicon')),
    display_name text not null default '',
    pfp_url text not null default '',
    email citext,
    first_seen_at timestamptz not null default now(),
    last_seen_at timestamptz not null default now()
);
create index known_accounts_email on known_accounts (email) where email is not null;

create table folders (
    id text primary key,
    name text not null,
    parent_id text references folders (id),
    owner uuid not null,
    created_by uuid not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    trashed_at timestamptz,
    trashed_by uuid,
    -- Gone for good: kept as a tombstone so ids are never reused.
    deleted_at timestamptz
);
create index folders_owner on folders (owner) where deleted_at is null;
create index folders_parent on folders (parent_id) where deleted_at is null;

create table sheets (
    id text primary key,
    title text not null,
    folder_id text references folders (id),
    owner uuid not null,
    created_by uuid not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    updated_by uuid not null,
    trashed_at timestamptz,
    trashed_by uuid,
    deleted_at timestamptz,
    public_enabled boolean not null default false,
    public_access text not null default 'view' check (public_access in ('view', 'comment', 'edit')),
    -- Text for search: the title and a bounded sample of cell text, refreshed with snapshots.
    search_text text not null default ''
);
create index sheets_owner on sheets (owner) where deleted_at is null;
create index sheets_folder on sheets (folder_id) where deleted_at is null;

-- A line is the main sheet or one of its copies; each has its own linear history.
create table lines (
    id text primary key,
    sheet_id text not null references sheets (id),
    is_main boolean not null,
    name text,
    -- The line this copy was made from, and the parent's version at that moment.
    parent_id text references lines (id),
    forked_at bigint,
    head bigint not null default 0,
    status text not null default 'open' check (status in ('open', 'merged', 'deleted')),
    created_by uuid not null,
    created_at timestamptz not null default now(),
    merged_at timestamptz,
    merged_by uuid,
    deleted_at timestamptz,
    deleted_by uuid
);
create unique index lines_one_main on lines (sheet_id) where is_main;
create index lines_sheet on lines (sheet_id);

-- Every operation, in order. Small ones inline, big ones in blob storage (MessagePack + zstd of calc_engine::Op).
create table ops (
    line_id text not null references lines (id),
    seq bigint not null,
    author uuid not null,
    at timestamptz not null default now(),
    kind text not null,
    summary text not null default '',
    op jsonb,
    op_blob text,
    inverse jsonb,
    inverse_blob text,
    cells_changed bigint not null default 0,
    client_id text,
    -- The undo bookkeeping: an undo op names the op it undid; a redo names the undo it reverted.
    undoes bigint,
    redoes bigint,
    primary key (line_id, seq),
    check ((op is not null) or (op_blob is not null))
);
create index ops_author on ops (line_id, author, seq);

create table snapshots (
    line_id text not null references lines (id),
    seq bigint not null,
    blob text not null,
    bytes bigint not null,
    created_at timestamptz not null default now(),
    primary key (line_id, seq)
);

create table version_names (
    line_id text not null references lines (id),
    seq bigint not null,
    name text not null,
    named_by uuid not null,
    named_at timestamptz not null default now(),
    primary key (line_id, seq)
);

create table members (
    item_kind text not null check (item_kind in ('sheet', 'folder')),
    item_id text not null,
    account uuid not null,
    role text not null check (role in ('editor', 'commenter', 'viewer')),
    invited_by uuid not null,
    created_at timestamptz not null default now(),
    primary key (item_kind, item_id, account)
);
create index members_account on members (account);

create table invites (
    item_kind text not null check (item_kind in ('sheet', 'folder')),
    item_id text not null,
    email citext not null,
    role text not null check (role in ('editor', 'commenter', 'viewer')),
    invited_by uuid not null,
    created_at timestamptz not null default now(),
    primary key (item_kind, item_id, email)
);

create table transfers (
    id text primary key,
    item_kind text not null check (item_kind in ('sheet', 'folder')),
    item_id text not null,
    from_account uuid not null,
    to_account uuid not null,
    status text not null default 'pending' check (status in ('pending', 'accepted', 'declined', 'cancelled')),
    created_at timestamptz not null default now(),
    decided_at timestamptz
);
create unique index transfers_one_pending on transfers (item_kind, item_id) where status = 'pending';

-- Comment threads are anchored by tab, row and column ids, so they move with their cells.
create table threads (
    id uuid primary key,
    sheet_id text not null references sheets (id),
    line_id text not null references lines (id),
    tab_id integer not null,
    -- Corner ids; null axes mean whole rows or columns; all null is the whole tab.
    r1 integer, c1 integer, r2 integer, c2 integer,
    original_range text,
    quote text not null default '',
    status text not null default 'open' check (status in ('open', 'resolved')),
    resolved_by uuid,
    resolved_at timestamptz,
    created_by uuid not null,
    created_at timestamptz not null default now()
);
create index threads_line on threads (line_id, status);

create table comments (
    id uuid primary key,
    thread_id uuid not null references threads (id),
    author uuid not null,
    body text not null,
    mentions uuid[] not null default '{}',
    created_at timestamptz not null default now(),
    edited_at timestamptz,
    deleted_at timestamptz
);
create index comments_thread on comments (thread_id, created_at);

create table reactions (
    comment_id uuid not null references comments (id),
    account uuid not null,
    emoji text not null,
    created_at timestamptz not null default now(),
    primary key (comment_id, account, emoji)
);

-- Notifications for Silicon Ting, written in the same transaction as what caused them.
create table outbox (
    id bigserial primary key,
    type text not null,
    recipient uuid not null,
    key text not null unique,
    body jsonb not null,
    status text not null default 'pending' check (status in ('pending', 'sent', 'failed', 'not_configured')),
    attempts integer not null default 0,
    next_attempt_at timestamptz not null default now(),
    last_error text,
    ting_id text,
    created_at timestamptz not null default now(),
    sent_at timestamptz
);
create index outbox_due on outbox (next_attempt_at) where status = 'pending';
