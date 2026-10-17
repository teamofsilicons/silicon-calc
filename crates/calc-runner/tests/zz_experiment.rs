use std::ffi::{CStr, CString};
use duckdb::{ffi, Connection};

unsafe fn open_raw(opts: &[(&str, &str)]) -> ffi::duckdb_database {
    unsafe {
        let mut cfg: ffi::duckdb_config = std::ptr::null_mut();
        assert_eq!(ffi::duckdb_create_config(&mut cfg), ffi::DuckDBSuccess);
        for (k, v) in opts {
            let (k, v) = (CString::new(*k).unwrap(), CString::new(*v).unwrap());
            let r = ffi::duckdb_set_config(cfg, k.as_ptr(), v.as_ptr());
            println!("set {:?}={:?} -> {}", k, v, r);
        }
        let mut db: ffi::duckdb_database = std::ptr::null_mut();
        let mut err: *mut std::os::raw::c_char = std::ptr::null_mut();
        let path = CString::new(":memory:").unwrap();
        let r = ffi::duckdb_open_ext(path.as_ptr(), &mut db, cfg, &mut err);
        if r != ffi::DuckDBSuccess { panic!("open: {:?}", CStr::from_ptr(err)); }
        ffi::duckdb_destroy_config(&mut cfg);
        db
    }
}

unsafe fn stmt_type(con: ffi::duckdb_connection, sql: &str) -> String {
    unsafe {
        let c = CString::new(sql).unwrap();
        let mut ex: ffi::duckdb_extracted_statements = std::ptr::null_mut();
        let n = ffi::duckdb_extract_statements(con, c.as_ptr(), &mut ex);
        if n == 0 {
            let e = ffi::duckdb_extract_statements_error(ex);
            let s = if e.is_null() { "(null)".to_string() } else { CStr::from_ptr(e).to_string_lossy().to_string() };
            ffi::duckdb_destroy_extracted(&mut ex);
            return format!("extract error: {s}");
        }
        let mut st: ffi::duckdb_prepared_statement = std::ptr::null_mut();
        let r = ffi::duckdb_prepare_extracted_statement(con, ex, 0, &mut st);
        let out = if r != ffi::DuckDBSuccess { format!("n={n} prepare error: {}", CStr::from_ptr(ffi::duckdb_prepare_error(st)).to_string_lossy()) } else { format!("n={n} type={}", ffi::duckdb_prepared_statement_type(st)) };
        ffi::duckdb_destroy_prepare(&mut st);
        ffi::duckdb_destroy_extracted(&mut ex);
        out
    }
}

#[test]
fn experiment() {
    unsafe {
        let db = open_raw(&[("enable_external_access", "false"), ("autoinstall_known_extensions", "false"), ("autoload_known_extensions", "false"), ("allow_community_extensions", "false"), ("temp_directory", ""), ("threads", "4"), ("memory_limit", "2GB"), ("lock_configuration", "true")]);
        let conn = Connection::open_from_raw(db).unwrap();
        conn.execute_batch("create table \"Sales Data\" (region varchar, amount double, d date); insert into \"Sales Data\" values ('EU', 1.5, '2026-10-17'), ('US', 2.5, null)").unwrap();
        let mut con: ffi::duckdb_connection = std::ptr::null_mut();
        assert_eq!(ffi::duckdb_connect(db, &mut con), ffi::DuckDBSuccess);
        for q in ["select * from \"sales data\"", "from \"Sales Data\"", "describe \"Sales Data\"", "show tables", "summarize \"Sales Data\"", "pivot \"Sales Data\" on region using sum(amount)", "explain select 1", "explain analyze select 1", "pragma table_info('Sales Data')", "call pragma_version()", "with x as (select 1) insert into \"Sales Data\" select 'a', 1, null", "values (1, 2)", "select 1; select 2", "set threads=1", "copy \"Sales Data\" to '/tmp/x.csv'", "select * from read_csv('/etc/passwd')", "attach '/tmp/x.db'", "install httpfs", "load httpfs", "select * from salse", "selec 1", "", "  -- just a comment\n", "update \"Sales Data\" set amount = 0", "create table t as select 1", "select * from duckdb_settings() limit 1", "select getenv('HOME')", "select * from glob('/*')", "select current_setting('enable_external_access')"] {
            println!("{q:?} => {}", stmt_type(con, q));
        }
        for q in ["select * from read_csv('/etc/passwd')", "copy \"Sales Data\" to '/tmp/calc-exp-x.csv'", "set threads=1", "select * from glob('/*')", "select getenv('HOME')", "install httpfs", "attach '/tmp/calc-exp.db'", "pragma enable_profiling", "select 0/0 as a, 1/0 as b, 1.0/0 as c, 'nan'::double as n", "select 1::hugeint as h, 1.25::decimal(10,2) as d, 'x'::varchar as s, interval 1 day as i, now() as tz, gen_random_uuid() as u, [1,2] as l, {'a': 1} as st, 'a'::enum('a','b') as e, time '13:45:00' as t, timestamp '2026-10-17 13:45:00' as ts, true as b, 10::tinyint as ti, 'abc'::blob as bl, 1::uhugeint as uh, 5::bit as bt"] {
            match conn.prepare(q) {
                Ok(mut st) => match st.query_arrow([]) {
                    Ok(arrow) => {
                        for b in arrow {
                            println!("{q:?} OK schema={:?}", b.schema().fields().iter().map(|f| format!("{}:{}", f.name(), f.data_type())).collect::<Vec<_>>());
                            println!("   {:?}", duckdb::arrow::util::pretty::pretty_format_batches(&[b]).map(|t| t.to_string()));
                        }
                    }
                    Err(e) => println!("{q:?} query error: {e}"),
                },
                Err(e) => println!("{q:?} prepare error: {e}"),
            }
        }
        ffi::duckdb_disconnect(&mut con);
        drop(conn);
        let mut db = db;
        ffi::duckdb_close(&mut db);
    }
}
