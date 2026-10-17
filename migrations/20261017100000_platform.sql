-- Platform: sharing, transfers, comments, search and the outbox, beyond the first schema.

-- An invite remembers whether its Carbon should be told (through Ting) when it becomes a membership.
alter table invites add column notify boolean not null default true;
create index invites_email on invites (email);

-- What a transfer was for, kept for when the item is gone.
alter table transfers add column title text not null default '';
create index transfers_to on transfers (to_account, created_at desc);
create index transfers_from on transfers (from_account, created_at desc);
create index transfers_item on transfers (item_kind, item_id);

-- A thread keeps its tab's name for when the tab is deleted and, for ranges up to 1,000 rows or columns, every row
-- and column id it covered, so it stays on whatever is left of its cells when some of them are deleted.
alter table threads add column tab_name text not null default '';
alter table threads add column row_ids integer[];
alter table threads add column col_ids integer[];
create index threads_sheet on threads (sheet_id);

-- The trash, by owner.
create index sheets_trash on sheets (owner, trashed_at) where trashed_at is not null and deleted_at is null;
create index folders_trash on folders (owner, trashed_at) where trashed_at is not null and deleted_at is null;

-- Deleting forever asks whether a blob is still used before removing it.
create index snapshots_blob on snapshots (blob);
create index ops_op_blob on ops (op_blob) where op_blob is not null;
create index ops_inverse_blob on ops (inverse_blob) where inverse_blob is not null;

-- Looking at what was sent to whom.
create index outbox_recipient on outbox (recipient, id);
