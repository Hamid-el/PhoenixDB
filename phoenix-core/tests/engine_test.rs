//! End-to-end tests of the execution engine: SQL in, rows out — including
//! catalog persistence, WAL crash recovery and transactions.

use tempfile::tempdir;

use phoenix_core::engine::{Engine, QueryResult, Session};
use phoenix_core::paging::error::DbError;
use phoenix_core::query::types::Value;

fn exec(engine: &Engine, session: &mut Session, sql: &str) -> QueryResult {
    engine
        .execute(sql, session)
        .unwrap_or_else(|e| panic!("'{}' failed: {}", sql, e))
}

fn rows_of(result: QueryResult) -> Vec<Vec<Value>> {
    match result {
        QueryResult::Rows { rows, .. } => rows,
        other => panic!("expected rows, got {:?}", other),
    }
}

#[test]
fn test_create_insert_select_roundtrip() {
    let dir = tempdir().unwrap();
    let engine = Engine::open(dir.path().join("test.db")).unwrap();
    let mut session = Session::new();

    exec(
        &engine,
        &mut session,
        "CREATE TABLE users (id INT, name VARCHAR);",
    );
    exec(&engine, &mut session, "INSERT INTO users VALUES (1, 'alice');");
    exec(&engine, &mut session, "INSERT INTO users VALUES (2, 'bob');");
    exec(&engine, &mut session, "INSERT INTO users VALUES (3, 'carol');");

    let result = exec(&engine, &mut session, "SELECT name FROM users WHERE id >= 2;");
    let QueryResult::Rows { columns, rows } = result else {
        panic!("expected rows");
    };
    assert_eq!(columns, vec!["name"]);
    assert_eq!(
        rows,
        vec![
            vec![Value::Varchar("bob".to_string())],
            vec![Value::Varchar("carol".to_string())],
        ]
    );
}

#[test]
fn test_select_star_order_by_desc() {
    let dir = tempdir().unwrap();
    let engine = Engine::open(dir.path().join("test.db")).unwrap();
    let mut session = Session::new();

    exec(&engine, &mut session, "CREATE TABLE t (id INT, v VARCHAR);");
    exec(&engine, &mut session, "INSERT INTO t VALUES (2, 'b');");
    exec(&engine, &mut session, "INSERT INTO t VALUES (1, 'a');");
    exec(&engine, &mut session, "INSERT INTO t VALUES (3, 'c');");

    let rows = rows_of(exec(&engine, &mut session, "SELECT * FROM t ORDER BY id DESC;"));
    assert_eq!(
        rows.iter().map(|r| r[0].clone()).collect::<Vec<_>>(),
        vec![Value::Int(3), Value::Int(2), Value::Int(1)]
    );
}

#[test]
fn test_where_tautology_and_arithmetic() {
    let dir = tempdir().unwrap();
    let engine = Engine::open(dir.path().join("test.db")).unwrap();
    let mut session = Session::new();

    exec(&engine, &mut session, "CREATE TABLE t (id INT);");
    for i in 1..=5 {
        exec(
            &engine,
            &mut session,
            &format!("INSERT INTO t VALUES ({});", i),
        );
    }

    // WHERE 1=1 is optimized away and must return everything.
    let rows = rows_of(exec(&engine, &mut session, "SELECT * FROM t WHERE 1 = 1;"));
    assert_eq!(rows.len(), 5);

    // Constant folding + runtime arithmetic on columns.
    let rows = rows_of(exec(
        &engine,
        &mut session,
        "SELECT id FROM t WHERE id * 2 > 2 + 4;",
    ));
    assert_eq!(
        rows,
        vec![vec![Value::Int(4)], vec![Value::Int(5)]]
    );

    // Contradiction short-circuits to an empty result.
    let rows = rows_of(exec(&engine, &mut session, "SELECT * FROM t WHERE 1 = 2;"));
    assert!(rows.is_empty());
}

#[test]
fn test_persistence_after_clean_shutdown() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");

    {
        let engine = Engine::open(&db_path).unwrap();
        let mut session = Session::new();
        exec(&engine, &mut session, "CREATE TABLE t (id INT, v VARCHAR);");
        exec(&engine, &mut session, "INSERT INTO t VALUES (1, 'persists');");
        engine.checkpoint().unwrap();
    }

    let engine = Engine::open(&db_path).unwrap();
    let mut session = Session::new();
    let rows = rows_of(exec(&engine, &mut session, "SELECT v FROM t WHERE id = 1;"));
    assert_eq!(rows, vec![vec![Value::Varchar("persists".to_string())]]);
}

#[test]
fn test_crash_recovery_from_wal() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");

    // No checkpoint: dropping the engine simulates a crash — committed
    // data exists only in the WAL.
    {
        let engine = Engine::open(&db_path).unwrap();
        let mut session = Session::new();
        exec(&engine, &mut session, "CREATE TABLE t (id INT, v VARCHAR);");
        exec(&engine, &mut session, "INSERT INTO t VALUES (1, 'wal');");
        exec(&engine, &mut session, "INSERT INTO t VALUES (2, 'replay');");
    }

    let engine = Engine::open(&db_path).unwrap();
    let mut session = Session::new();
    let rows = rows_of(exec(&engine, &mut session, "SELECT v FROM t ORDER BY id;"));
    assert_eq!(
        rows,
        vec![
            vec![Value::Varchar("wal".to_string())],
            vec![Value::Varchar("replay".to_string())],
        ]
    );

    // Recovery must also restore next_row_id: new inserts must not collide.
    exec(&engine, &mut session, "INSERT INTO t VALUES (3, 'after');");
    let rows = rows_of(exec(&engine, &mut session, "SELECT * FROM t;"));
    assert_eq!(rows.len(), 3);
}

#[test]
fn test_crash_recovery_with_many_rows_and_splits() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let total = 200; // enough to force several leaf splits (15 keys/leaf)

    {
        let engine = Engine::open(&db_path).unwrap();
        let mut session = Session::new();
        exec(&engine, &mut session, "CREATE TABLE t (id INT);");
        for i in 0..total {
            exec(
                &engine,
                &mut session,
                &format!("INSERT INTO t VALUES ({});", i),
            );
        }
    }

    let engine = Engine::open(&db_path).unwrap();
    let mut session = Session::new();
    let rows = rows_of(exec(&engine, &mut session, "SELECT * FROM t;"));
    assert_eq!(rows.len(), total);
}

#[test]
fn test_transaction_commit_makes_data_visible() {
    let dir = tempdir().unwrap();
    let engine = Engine::open(dir.path().join("test.db")).unwrap();
    let mut session = Session::new();

    exec(&engine, &mut session, "CREATE TABLE t (id INT);");
    exec(&engine, &mut session, "BEGIN;");
    exec(&engine, &mut session, "INSERT INTO t VALUES (1);");
    exec(&engine, &mut session, "INSERT INTO t VALUES (2);");

    // Deferred update: another session sees nothing before COMMIT.
    let mut other = Session::new();
    let rows = rows_of(exec(&engine, &mut other, "SELECT * FROM t;"));
    assert!(rows.is_empty());

    exec(&engine, &mut session, "COMMIT;");
    let rows = rows_of(exec(&engine, &mut other, "SELECT * FROM t;"));
    assert_eq!(rows.len(), 2);
}

#[test]
fn test_transaction_rollback_discards() {
    let dir = tempdir().unwrap();
    let engine = Engine::open(dir.path().join("test.db")).unwrap();
    let mut session = Session::new();

    exec(&engine, &mut session, "CREATE TABLE t (id INT);");
    exec(&engine, &mut session, "BEGIN TRANSACTION;");
    exec(&engine, &mut session, "INSERT INTO t VALUES (42);");
    exec(&engine, &mut session, "ROLLBACK;");

    let rows = rows_of(exec(&engine, &mut session, "SELECT * FROM t;"));
    assert!(rows.is_empty());

    // The session is usable again afterwards.
    exec(&engine, &mut session, "INSERT INTO t VALUES (7);");
    let rows = rows_of(exec(&engine, &mut session, "SELECT * FROM t;"));
    assert_eq!(rows, vec![vec![Value::Int(7)]]);
}

#[test]
fn test_transaction_control_errors() {
    let dir = tempdir().unwrap();
    let engine = Engine::open(dir.path().join("test.db")).unwrap();
    let mut session = Session::new();

    // COMMIT/ROLLBACK without BEGIN
    assert!(matches!(
        engine.execute("COMMIT;", &mut session),
        Err(DbError::Transaction(_))
    ));
    assert!(matches!(
        engine.execute("ROLLBACK;", &mut session),
        Err(DbError::Transaction(_))
    ));

    // Nested BEGIN
    exec(&engine, &mut session, "BEGIN;");
    assert!(matches!(
        engine.execute("BEGIN;", &mut session),
        Err(DbError::Transaction(_))
    ));

    // CREATE TABLE inside a transaction is rejected
    assert!(matches!(
        engine.execute("CREATE TABLE t (id INT);", &mut session),
        Err(DbError::Transaction(_))
    ));
    exec(&engine, &mut session, "ROLLBACK;");
}

#[test]
fn test_analyzer_errors_surface() {
    let dir = tempdir().unwrap();
    let engine = Engine::open(dir.path().join("test.db")).unwrap();
    let mut session = Session::new();

    exec(&engine, &mut session, "CREATE TABLE users (id INT, name VARCHAR);");

    assert!(matches!(
        engine.execute("SELECT * FROM ghosts;", &mut session),
        Err(DbError::TableNotFound(_))
    ));
    assert!(matches!(
        engine.execute("SELECT age FROM users;", &mut session),
        Err(DbError::ColumnNotFound(..))
    ));
    assert!(matches!(
        engine.execute("INSERT INTO users VALUES ('x', 'y');", &mut session),
        Err(DbError::TypeError(_))
    ));
    assert!(matches!(
        engine.execute("INSERT INTO users VALUES (1);", &mut session),
        Err(DbError::Sql(_))
    ));
    assert!(matches!(
        engine.execute("CREATE TABLE users (id INT);", &mut session),
        Err(DbError::TableExists(_))
    ));
    assert!(matches!(
        engine.execute("SELEKT * FROM users;", &mut session),
        Err(DbError::Sql(_))
    ));
}

#[test]
fn test_multiple_tables_independent() {
    let dir = tempdir().unwrap();
    let engine = Engine::open(dir.path().join("test.db")).unwrap();
    let mut session = Session::new();

    exec(&engine, &mut session, "CREATE TABLE a (x INT);");
    exec(&engine, &mut session, "CREATE TABLE b (y VARCHAR);");
    exec(&engine, &mut session, "INSERT INTO a VALUES (1);");
    exec(&engine, &mut session, "INSERT INTO b VALUES ('one');");

    assert_eq!(rows_of(exec(&engine, &mut session, "SELECT * FROM a;")).len(), 1);
    assert_eq!(
        rows_of(exec(&engine, &mut session, "SELECT * FROM b;")),
        vec![vec![Value::Varchar("one".to_string())]]
    );
}

#[test]
fn test_record_too_large_rejected_cleanly() {
    let dir = tempdir().unwrap();
    let engine = Engine::open(dir.path().join("test.db")).unwrap();
    let mut session = Session::new();

    exec(&engine, &mut session, "CREATE TABLE t (v VARCHAR);");
    let big = "x".repeat(1000);
    let result = engine.execute(&format!("INSERT INTO t VALUES ('{}');", big), &mut session);
    assert!(matches!(result, Err(DbError::RecordTooLarge { .. })));

    // The failed insert must not have poisoned the engine.
    exec(&engine, &mut session, "INSERT INTO t VALUES ('small');");
    assert_eq!(rows_of(exec(&engine, &mut session, "SELECT * FROM t;")).len(), 1);
}
