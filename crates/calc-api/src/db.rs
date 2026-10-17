//! The Postgres pool and migrations, and small helpers every module shares.

use std::{collections::HashSet, time::Duration};

use sqlx::{PgConnection, PgPool, postgres::PgPoolOptions};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::error::{ApiError, ApiResult};

pub async fn connect(url: &str) -> anyhow::Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(32)
        .acquire_timeout(Duration::from_secs(10))
        .connect(url)
        .await?;
    sqlx::migrate!("../../migrations").run(&pool).await?;
    Ok(pool)
}

/// A time as every answer shows it: RFC 3339 in UTC.
pub fn rfc3339(t: OffsetDateTime) -> String {
    t.to_offset(time::UtcOffset::UTC).format(&Rfc3339).unwrap_or_default()
}

/// `rfc3339` for optional times (`null` when absent).
pub fn rfc3339_opt(t: Option<OffsetDateTime>) -> serde_json::Value {
    t.map_or(serde_json::Value::Null, |t| serde_json::Value::String(rfc3339(t)))
}

/// Whether an error is a unique-constraint violation (an id or a key that is taken).
pub fn is_unique_violation(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(d) if d.is_unique_violation())
}

/// A text for `ILIKE` that matches `text` anywhere, with `%`, `_` and `\` taken literally.
pub fn like_anywhere(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('%');
    for c in text.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Single-column foreign keys that point at `table(column)`: `(referencing table, its column)`. The table comes back
/// as Postgres writes it (`regclass`), ready to put in SQL.
async fn references_to(conn: &mut PgConnection, table: &str, column: &str) -> ApiResult<Vec<(String, String)>> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "select c.conrelid::regclass::text, a.attname::text
         from pg_constraint c
         join pg_attribute a on a.attrelid = c.conrelid and a.attnum = c.conkey[1]
         join pg_attribute r on r.attrelid = c.confrelid and r.attnum = c.confkey[1]
         where c.contype = 'f' and c.confrelid = $1::regclass and r.attname = $2 and cardinality(c.conkey) = 1
         order by 1, 2",
    )
    .bind(table)
    .bind(column)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows)
}

/// Deletes, for good, every row of every table that points at the rows of `table` chosen by `selection` (a
/// `select <column> from …` using `$1`, bound to `ids`), and the rows that point at those, all the way down. The
/// chosen rows themselves stay. Tables are found from their foreign keys, so tables other modules add later are
/// covered without this code knowing them. A table pointing at itself (a copy's parent line) is deleted in one
/// statement, which Postgres allows.
pub async fn purge_references(
    conn: &mut PgConnection,
    table: &str,
    column: &str,
    selection: &str,
    ids: &[String],
) -> ApiResult<u64> {
    purge_level(conn, table, column, selection.to_owned(), ids, 0).await
}

fn purge_level<'a>(
    conn: &'a mut PgConnection,
    table: &'a str,
    column: &'a str,
    selection: String,
    ids: &'a [String],
    depth: u32,
) -> futures::future::BoxFuture<'a, ApiResult<u64>> {
    Box::pin(async move {
        if depth > 12 {
            return Err(ApiError::internal(format!("deleting rows that point at {table} went more than 12 tables deep")));
        }
        let mut deleted = 0;
        for (child, child_col) in references_to(conn, table, column).await? {
            if child == table {
                continue;
            }
            let rows = format!("{} in ({selection})", quote_ident(&child_col));
            // Whatever points at the child's rows goes first; each referenced column is followed separately.
            let referenced: Vec<(String,)> = sqlx::query_as(
                "select distinct r.attname::text
                 from pg_constraint c
                 join pg_attribute r on r.attrelid = c.confrelid and r.attnum = c.confkey[1]
                 where c.contype = 'f' and c.confrelid = $1::regclass and cardinality(c.conkey) = 1",
            )
            .bind(&child)
            .fetch_all(&mut *conn)
            .await?;
            for (col,) in referenced {
                let sub = format!("select {} from {child} where {rows}", quote_ident(&col));
                deleted += purge_level(conn, &child, &col, sub, ids, depth + 1).await?;
            }
            let res = sqlx::query(sqlx::AssertSqlSafe(format!("delete from {child} where {rows}")))
                .bind(ids)
                .execute(&mut *conn)
                .await?;
            deleted += res.rows_affected();
        }
        Ok(deleted)
    })
}

/// Which of `keys` some row still points at, in any column named `blob` or ending in `_blob` (the convention for
/// columns holding blob keys), so a blob is only removed when nothing needs it.
pub async fn blob_keys_in_use(pool: &PgPool, keys: &[String]) -> ApiResult<HashSet<String>> {
    let mut used = HashSet::new();
    if keys.is_empty() {
        return Ok(used);
    }
    let columns: Vec<(String, String)> = sqlx::query_as(
        "select quote_ident(table_schema) || '.' || quote_ident(table_name), column_name::text
         from information_schema.columns
         where table_schema = current_schema()
           and (column_name = 'blob' or column_name like '%\\_blob' escape '\\')
           and data_type in ('text', 'character varying')",
    )
    .fetch_all(pool)
    .await?;
    for (table, column) in columns {
        let found: Vec<(String,)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "select distinct {col} from {table} where {col} = any($1)",
            col = quote_ident(&column)
        )))
        .bind(keys)
        .fetch_all(pool)
        .await?;
        used.extend(found.into_iter().map(|f| f.0));
    }
    Ok(used)
}
