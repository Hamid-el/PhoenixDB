use std::path::{Path, PathBuf};
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use log::info;

use crate::catalog::{Catalog, Column, TableSchema};
use crate::paging::buffer_pool::BufferPoolManager;
use crate::paging::disk_manager::DiskManager;
use crate::paging::error::{DbError, Result};
use crate::paging::replacement::ClockStrategy;
use crate::query::scan::TableScan;
use crate::query::types::Value;
use crate::query::Operator;
use crate::sql::analyzer::analyze;
use crate::sql::ast::{DataType, Expression, Statement};
use crate::sql::optimizer::optimize;
use crate::sql::parser::parse_statement;
use crate::storage::btree::BPlusTree;
use crate::storage::tuple::{decode_row, encode_row, RECORD_SIZE};
use crate::wal::Wal;

const DEFAULT_POOL_SIZE: usize = 1024;
/// Once the WAL grows past this, the next commit triggers a checkpoint.
const WAL_CHECKPOINT_BYTES: u64 = 4 * 1024 * 1024;

type Tree<'a> = BPlusTree<'a, ClockStrategy, RECORD_SIZE>;

#[derive(Debug, Clone, PartialEq)]
pub enum QueryResult {
    Ok,
    Inserted(usize),
    /// Result set of a SELECT
    Rows {
        columns: Vec<String>,
        rows: Vec<Vec<Value>>,
    },
}

/// Per-connection state. Holds the buffered operations of an open
/// transaction; dropping the session (client disconnect) discards them,
/// which is an implicit ROLLBACK.
#[derive(Default)]
pub struct Session {
    txn: Option<Vec<TxnOp>>,
}

impl Session {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn in_transaction(&self) -> bool {
        self.txn.is_some()
    }
}

enum TxnOp {
    Insert { table: String, values: Vec<Value> },
}

pub struct Engine {
    inner: RwLock<Inner>,
}

struct Inner {
    bpm: BufferPoolManager<ClockStrategy>,
    catalog: Catalog,
    wal: Wal,
    catalog_path: PathBuf,
    /// Set when a mutation failed halfway through applying to the buffer
    /// pool: the in-memory state may be inconsistent and must not be
    /// committed. A restart recovers cleanly from the WAL.
    poisoned: bool,
}

impl Engine {
    /// Opens (or creates) a database. `db_path` is the data file; the WAL
    /// and catalog live next to it as `<name>.wal` / `<name>.catalog`.
    /// Runs crash recovery from the WAL before returning.
    pub fn open(db_path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_pool_size(db_path, DEFAULT_POOL_SIZE)
    }

    pub fn open_with_pool_size(db_path: impl AsRef<Path>, pool_size: usize) -> Result<Self> {
        let db_path = db_path.as_ref().to_path_buf();
        let wal_path = db_path.with_extension("wal");
        let catalog_path = db_path.with_extension("catalog");

        let disk_manager = DiskManager::new(&db_path)?;

        // replay committed page images from the WAL into
        // the data file and restore the catalog snapshot from the last
        // commit record (the catalog file itself may be stale).
        let recovered = Wal::read_committed(&wal_path)?;
        let did_recover = recovered.is_some();
        let catalog = match recovered {
            Some(state) => {
                info!(
                    "Recovering {} committed page(s) from WAL",
                    state.pages.len()
                );
                for (page_id, data) in &state.pages {
                    disk_manager.write_page_raw(*page_id, data)?;
                }
                disk_manager.sync()?;
                let catalog = Catalog::from_bytes(&state.catalog_bytes)?;
                catalog.save(&catalog_path)?;
                catalog
            }
            None => Catalog::load(&catalog_path)?,
        };

        let wal = Wal::open(&wal_path)?;
        if did_recover {
            // Data file and catalog are durable now; the WAL is redundant.
            wal.truncate()?;
        }

        let bpm = BufferPoolManager::new_with_policy(
            pool_size,
            disk_manager,
            ClockStrategy::new(pool_size),
            true, // no-steal: required for redo-only recovery correctness
        );

        Ok(Self {
            inner: RwLock::new(Inner {
                bpm,
                catalog,
                wal,
                catalog_path,
                poisoned: false,
            }),
        })
    }

    /// Parses, analyzes, optimizes and executes one SQL statement.
    pub fn execute(&self, sql: &str, session: &mut Session) -> Result<QueryResult> {
        let statement = parse_statement(sql).map_err(|e| DbError::Sql(e.to_string()))?;

        match statement {
            Statement::Begin => {
                if session.txn.is_some() {
                    return Err(DbError::Transaction(
                        "a transaction is already open".to_string(),
                    ));
                }
                session.txn = Some(Vec::new());
                Ok(QueryResult::Ok)
            }
            Statement::Rollback => {
                if session.txn.take().is_none() {
                    return Err(DbError::Transaction("no open transaction".to_string()));
                }
                Ok(QueryResult::Ok)
            }
            Statement::Commit => {
                let Some(ops) = session.txn.take() else {
                    return Err(DbError::Transaction("no open transaction".to_string()));
                };
                if ops.is_empty() {
                    return Ok(QueryResult::Ok);
                }
                let mut inner = self.write_inner()?;
                inner.check_poisoned()?;
                inner.apply_and_log(&ops)?;
                Ok(QueryResult::Ok)
            }
            Statement::CreateTable { .. } => {
                if session.txn.is_some() {
                    return Err(DbError::Transaction(
                        "CREATE TABLE inside an explicit transaction is not supported; \
                         COMMIT or ROLLBACK first"
                            .to_string(),
                    ));
                }
                let mut inner = self.write_inner()?;
                inner.check_poisoned()?;
                analyze(&statement, &inner.catalog)?;
                let Statement::CreateTable { name, columns } = statement else {
                    unreachable!()
                };
                inner.create_table(name, columns)?;
                Ok(QueryResult::Ok)
            }
            Statement::Insert { .. } => {
                if session.txn.is_some() {
                    // validate at COMMIT
                    {
                        let inner = self.read_inner()?;
                        analyze(&statement, &inner.catalog)?;
                    }
                    let Statement::Insert { table_name, values } = statement else {
                        unreachable!()
                    };
                    session.txn.as_mut().unwrap().push(TxnOp::Insert {
                        table: table_name,
                        values,
                    });
                    Ok(QueryResult::Inserted(1))
                } else {
                    let mut inner = self.write_inner()?;
                    inner.check_poisoned()?;
                    analyze(&statement, &inner.catalog)?;
                    let Statement::Insert { table_name, values } = statement else {
                        unreachable!()
                    };
                    inner.apply_and_log(&[TxnOp::Insert {
                        table: table_name,
                        values,
                    }])?;
                    Ok(QueryResult::Inserted(1))
                }
            }
            Statement::Select { .. } => {
                let inner = self.read_inner()?;
                analyze(&statement, &inner.catalog)?;
                let statement = optimize(statement);
                let Statement::Select {
                    projection,
                    table_name,
                    where_clause,
                    order_by,
                } = statement
                else {
                    unreachable!()
                };
                inner.run_select(
                    &projection,
                    &table_name,
                    where_clause.as_ref(),
                    order_by.as_ref(),
                )
            }
        }
    }

    /// Flushes all pages and the catalog to disk, then truncates the WAL.
    /// Call for a clean shutdown; after a crash the WAL replays instead.
    pub fn checkpoint(&self) -> Result<()> {
        let mut inner = self.write_inner()?;
        inner.checkpoint()
    }

    fn read_inner(&self) -> Result<RwLockReadGuard<'_, Inner>> {
        self.inner
            .read()
            .map_err(|e| DbError::Internal(e.to_string()))
    }

    fn write_inner(&self) -> Result<RwLockWriteGuard<'_, Inner>> {
        self.inner
            .write()
            .map_err(|e| DbError::Internal(e.to_string()))
    }
}

impl Inner {
    fn check_poisoned(&self) -> Result<()> {
        if self.poisoned {
            return Err(DbError::Internal(
                "engine is poisoned after a failed write; restart the database \
                 to recover from the WAL"
                    .to_string(),
            ));
        }
        Ok(())
    }

    fn create_table(&mut self, name: String, columns: Vec<(String, DataType)>) -> Result<()> {
        let result = self.create_table_inner(name, columns);
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }

    fn create_table_inner(
        &mut self,
        name: String,
        columns: Vec<(String, DataType)>,
    ) -> Result<()> {
        let root_page_id = {
            let tree = Tree::create(&self.bpm)?;
            tree.root_page_id()
        };

        self.catalog.add_table(TableSchema {
            name,
            columns: columns
                .into_iter()
                .map(|(name, data_type)| Column { name, data_type })
                .collect(),
            root_page_id,
            next_row_id: 1,
            indexes: Vec::new(),
        })?;

        self.log_commit()
    }

    /// Applies a batch of buffered operations as one atomic transaction:
    /// everything is validated/encoded first, then applied to the buffer
    /// pool, then sealed with a single WAL commit record.
    fn apply_and_log(&mut self, ops: &[TxnOp]) -> Result<()> {
        // Encode all rows up front: a validation failure aborts before
        // anything has touched the storage layer.
        let mut encoded = Vec::with_capacity(ops.len());
        for op in ops {
            let TxnOp::Insert { values, .. } = op;
            encoded.push(encode_row(values)?);
        }

        let result = self.apply_ops(ops, &encoded);
        if result.is_err() {
            // Pages may hold a half-applied transaction. Nothing of it is
            // in the WAL, so a restart recovers the last consistent state.
            self.poisoned = true;
        }
        result
    }

    fn apply_ops(&mut self, ops: &[TxnOp], encoded: &[[u8; RECORD_SIZE]]) -> Result<()> {
        for (op, record) in ops.iter().zip(encoded) {
            let TxnOp::Insert { table, .. } = op;
            self.insert_record(table, record)?;
        }
        self.log_commit()
    }

    fn insert_record(&mut self, table: &str, record: &[u8; RECORD_SIZE]) -> Result<()> {
        let (root_page_id, row_id) = {
            let schema = self
                .catalog
                .get_table(table)
                .ok_or_else(|| DbError::TableNotFound(table.to_string()))?;
            (schema.root_page_id, schema.next_row_id)
        };

        let new_root = {
            let tree = Tree::open(&self.bpm, root_page_id);
            tree.insert(row_id, record)?;
            tree.root_page_id()
        };

        let schema = self.catalog.get_table_mut(table).expect("checked above");
        schema.root_page_id = new_root;
        schema.next_row_id = row_id + 1;
        Ok(())
    }

    /// Seals the current buffer-pool changes with a WAL commit record
    /// (write-ahead: the fsync here is what makes the transaction durable).
    fn log_commit(&mut self) -> Result<()> {
        let pages = self.bpm.collect_dirty_unlogged()?;
        let catalog_bytes = self.catalog.to_bytes();
        self.wal.append_commit(&pages, &catalog_bytes)?;

        if self.wal.size()? > WAL_CHECKPOINT_BYTES {
            self.checkpoint()?;
        }
        Ok(())
    }

    fn checkpoint(&mut self) -> Result<()> {
        self.bpm.flush_all()?;
        self.bpm.sync()?;
        self.catalog.save(&self.catalog_path)?;
        self.wal.truncate()?;
        info!("Checkpoint complete");
        Ok(())
    }

    fn run_select(
        &self,
        projection: &[String],
        table_name: &str,
        where_clause: Option<&Expression>,
        order_by: Option<&(String, bool)>,
    ) -> Result<QueryResult> {
        let schema = self
            .catalog
            .get_table(table_name)
            .ok_or_else(|| DbError::TableNotFound(table_name.to_string()))?;

        let (columns, indices): (Vec<String>, Vec<usize>) =
            if projection.len() == 1 && projection[0] == "*" {
                (
                    schema.columns.iter().map(|c| c.name.clone()).collect(),
                    (0..schema.columns.len()).collect(),
                )
            } else {
                let mut names = Vec::with_capacity(projection.len());
                let mut indices = Vec::with_capacity(projection.len());
                for name in projection {
                    let idx = schema.column_index(name).ok_or_else(|| {
                        DbError::ColumnNotFound(name.clone(), schema.name.clone())
                    })?;
                    names.push(schema.columns[idx].name.clone());
                    indices.push(idx);
                }
                (names, indices)
            };

        // The optimizer folds contradictions (e.g. WHERE 1 = 2) down to a
        // constant FALSE: skip the scan entirely.
        if let Some(Expression::Literal(Value::Bool(false))) = where_clause {
            return Ok(QueryResult::Rows {
                columns,
                rows: Vec::new(),
            });
        }

        let mut rows: Vec<Vec<Value>> = Vec::new();
        let mut scan = TableScan::<ClockStrategy, RECORD_SIZE>::new(&self.bpm, schema.root_page_id);
        scan.open()?;
        while let Some(record) = scan.next()? {
            // The cursor prepends the 8-byte row id;
            // the tuple follows
            let row = decode_row(&record[8..])?;

            if let Some(expression) = where_clause {
                match eval_expression(expression, &row, schema)? {
                    Value::Bool(true) => {}
                    Value::Bool(false) => continue,
                    other => {
                        return Err(DbError::TypeError(format!(
                            "WHERE clause evaluated to non-boolean value {}",
                            other
                        )));
                    }
                }
            }
            rows.push(row);
        }
        scan.close()?;

        if let Some((column, ascending)) = order_by {
            let idx = schema.column_index(column).ok_or_else(|| {
                DbError::ColumnNotFound(column.clone(), schema.name.clone())
            })?;
            if *ascending {
                rows.sort_by(|a, b| a[idx].cmp(&b[idx]));
            } else {
                rows.sort_by(|a, b| b[idx].cmp(&a[idx]));
            }
        }

        let rows = rows
            .into_iter()
            .map(|row| indices.iter().map(|&i| row[i].clone()).collect())
            .collect();

        Ok(QueryResult::Rows { columns, rows })
    }
}

fn eval_expression(
    expression: &Expression,
    row: &[Value],
    schema: &TableSchema,
) -> Result<Value> {
    match expression {
        Expression::Literal(value) => Ok(value.clone()),
        Expression::Identifier(name) => {
            let idx = schema
                .column_index(name)
                .ok_or_else(|| DbError::ColumnNotFound(name.clone(), schema.name.clone()))?;
            row.get(idx)
                .cloned()
                .ok_or_else(|| DbError::Internal("row is missing a column".to_string()))
        }
        Expression::BinaryOp { left, op, right } => {
            let left = eval_expression(left, row, schema)?;
            let right = eval_expression(right, row, schema)?;
            eval_binary_op(&left, op, &right)
        }
    }
}

fn eval_binary_op(left: &Value, op: &str, right: &Value) -> Result<Value> {
    let type_error = || {
        DbError::TypeError(format!(
            "cannot apply '{}' to {} and {}",
            op, left, right
        ))
    };

    match op {
        "+" | "-" | "*" | "/" => {
            let (Value::Int(a), Value::Int(b)) = (left, right) else {
                return Err(type_error());
            };
            let result = match op {
                "+" => a.checked_add(*b),
                "-" => a.checked_sub(*b),
                "*" => a.checked_mul(*b),
                "/" => {
                    if *b == 0 {
                        return Err(DbError::Sql("division by zero".to_string()));
                    }
                    a.checked_div(*b)
                }
                _ => unreachable!(),
            };
            result
                .map(Value::Int)
                .ok_or_else(|| DbError::Sql("integer overflow in expression".to_string()))
        }
        "=" => Ok(Value::Bool(left == right)),
        "<>" => Ok(Value::Bool(left != right)),
        "<" | "<=" | ">" | ">=" => {
            if std::mem::discriminant(left) != std::mem::discriminant(right) {
                return Err(type_error());
            }
            let ord = left.cmp(right);
            let result = match op {
                "<" => ord.is_lt(),
                "<=" => ord.is_le(),
                ">" => ord.is_gt(),
                ">=" => ord.is_ge(),
                _ => unreachable!(),
            };
            Ok(Value::Bool(result))
        }
        "AND" | "OR" => {
            let (Value::Bool(a), Value::Bool(b)) = (left, right) else {
                return Err(type_error());
            };
            Ok(Value::Bool(if op == "AND" { *a && *b } else { *a || *b }))
        }
        other => Err(DbError::Internal(format!(
            "unknown binary operator '{}'",
            other
        ))),
    }
}
