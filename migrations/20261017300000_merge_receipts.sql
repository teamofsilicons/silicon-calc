-- Accepted source changes and stable identity mappings for merging an open copy again.
-- The original fork and its history remain immutable.
create table merge_receipts (
    line_id text primary key references lines(id),
    source_blob text not null,
    ids jsonb not null,
    parent_version bigint not null,
    copy_version bigint not null,
    revision bigint not null default 1
);
