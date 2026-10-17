-- Lines: what each operation changed, for cell history, overwrite reports, versions and live replays.

-- How viewers were told about an operation (docs/API.md `Change` objects), and what versions say about it:
-- `{"cells": {"<tab name>": n}, "phrases": ["inserted 2 rows in Sales", …]}`.
alter table ops add column changes jsonb;
alter table ops add column facts jsonb;
-- An operation made of several edits, each applied on its own (replaying applies them the same way).
alter table ops add column staged boolean not null default false;

-- Every cell an operation changed (by tab, row and column ids, so it follows the cell as rows move), with what it
-- held before and after. Operations writing more than 2,000 cells are kept in op_tabs as one bulk change instead.
create table op_cells (
    line_id text not null references lines (id),
    tab_id integer not null,
    row_id bigint not null,
    col_id bigint not null,
    seq bigint not null,
    before jsonb not null,
    after jsonb not null,
    primary key (line_id, tab_id, row_id, col_id, seq)
);
create index op_cells_seq on op_cells (line_id, seq);

-- The tabs each operation touched. Bulk changes keep the area they covered (corner ids; all null is the whole tab).
create table op_tabs (
    line_id text not null references lines (id),
    seq bigint not null,
    tab_id integer not null,
    bulk boolean not null default false,
    r1 bigint,
    c1 bigint,
    r2 bigint,
    c2 bigint,
    primary key (line_id, seq, tab_id)
);
create index op_tabs_bulk on op_tabs (line_id, tab_id, seq) where bulk;

-- Copies by sheet and parent, for the copy tree.
create index lines_parent on lines (parent_id) where parent_id is not null;
