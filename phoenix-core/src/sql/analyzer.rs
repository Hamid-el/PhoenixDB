
use crate::catalog::{Catalog, TableSchema};
use crate::paging::error::{DbError, Result};
use crate::query::types::Value;
use crate::sql::ast::{DataType, Expression, Statement};

/// Semantic logic-analysis: the layer between the parser and the query planner.

/// The type an expression evaluates to. Superset of the storable column
/// types (`DataType`) because expressions can also produce booleans.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExprType {
    Int,
    Varchar,
    Bool,
}

impl std::fmt::Display for ExprType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExprType::Int => write!(f, "INT"),
            ExprType::Varchar => write!(f, "VARCHAR"),
            ExprType::Bool => write!(f, "BOOL"),
        }
    }
}

impl From<&DataType> for ExprType {
    fn from(dt: &DataType) -> Self {
        match dt {
            DataType::Int => ExprType::Int,
            DataType::Varchar => ExprType::Varchar,
        }
    }
}

fn type_of_value(value: &Value) -> ExprType {
    match value {
        Value::Int(_) => ExprType::Int,
        Value::Varchar(_) => ExprType::Varchar,
        Value::Bool(_) => ExprType::Bool,
    }
}

/// Validates a statement against the catalog. Returns `Ok(())` if the
/// statement is well-formed and executable.
pub fn analyze(statement: &Statement, catalog: &Catalog) -> Result<()> {
    match statement {
        Statement::CreateTable { name, columns } => analyze_create_table(name, columns, catalog),
        Statement::Insert { table_name, values } => analyze_insert(table_name, values, catalog),
        Statement::Select {
            projection,
            table_name,
            where_clause,
            order_by,
        } => analyze_select(projection, table_name, where_clause, order_by, catalog),
        Statement::Begin | Statement::Commit | Statement::Rollback => Ok(()),
    }
}

fn analyze_create_table(
    name: &str,
    columns: &[(String, DataType)],
    catalog: &Catalog,
) -> Result<()> {
    if catalog.get_table(name).is_some() {
        return Err(DbError::TableExists(name.to_string()));
    }
    if columns.is_empty() {
        return Err(DbError::Sql(format!(
            "table '{}' must have at least one column",
            name
        )));
    }
    for (i, (column_name, _)) in columns.iter().enumerate() {
        let duplicate = columns[..i]
            .iter()
            .any(|(other, _)| other.eq_ignore_ascii_case(column_name));
        if duplicate {
            return Err(DbError::Sql(format!(
                "duplicate column '{}' in table '{}'",
                column_name, name
            )));
        }
    }
    Ok(())
}

fn analyze_insert(table_name: &str, values: &[Value], catalog: &Catalog) -> Result<()> {
    let schema = catalog
        .get_table(table_name)
        .ok_or_else(|| DbError::TableNotFound(table_name.to_string()))?;

    if values.len() != schema.columns.len() {
        return Err(DbError::Sql(format!(
            "table '{}' has {} columns but {} values were supplied",
            schema.name,
            schema.columns.len(),
            values.len()
        )));
    }

    for (value, column) in values.iter().zip(&schema.columns) {
        let expected = ExprType::from(&column.data_type);
        let actual = type_of_value(value);
        if expected != actual {
            return Err(DbError::TypeError(format!(
                "column '{}' expects {} but got {} ({})",
                column.name, expected, actual, value
            )));
        }
    }
    Ok(())
}

fn analyze_select(
    projection: &[String],
    table_name: &str,
    where_clause: &Option<Expression>,
    order_by: &Option<(String, bool)>,
    catalog: &Catalog,
) -> Result<()> {
    let schema = catalog
        .get_table(table_name)
        .ok_or_else(|| DbError::TableNotFound(table_name.to_string()))?;

    for column in projection {
        if column != "*" && schema.column(column).is_none() {
            return Err(DbError::ColumnNotFound(
                column.clone(),
                schema.name.clone(),
            ));
        }
    }

    if let Some(expression) = where_clause {
        let expr_type = type_check_expression(expression, schema)?;
        if expr_type != ExprType::Bool {
            return Err(DbError::TypeError(format!(
                "WHERE clause must be a boolean expression, got {}",
                expr_type
            )));
        }
    }

    if let Some((column, _)) = order_by {
        if schema.column(column).is_none() {
            return Err(DbError::ColumnNotFound(
                column.clone(),
                schema.name.clone(),
            ));
        }
    }

    Ok(())
}

/// Type-checks an expression against a table schema
/// and returns the type it evaluates to
pub fn type_check_expression(expression: &Expression, schema: &TableSchema) -> Result<ExprType> {
    match expression {
        Expression::Literal(value) => Ok(type_of_value(value)),
        Expression::Identifier(name) => schema
            .column(name)
            .map(|c| ExprType::from(&c.data_type))
            .ok_or_else(|| DbError::ColumnNotFound(name.clone(), schema.name.clone())),
        Expression::BinaryOp { left, op, right } => {
            let left_type = type_check_expression(left, schema)?;
            let right_type = type_check_expression(right, schema)?;

            match op.as_str() {
                "+" | "-" | "*" | "/" => {
                    if left_type == ExprType::Int && right_type == ExprType::Int {
                        Ok(ExprType::Int)
                    } else {
                        Err(DbError::TypeError(format!(
                            "operator '{}' requires INT operands, got {} and {}",
                            op, left_type, right_type
                        )))
                    }
                }
                "=" | "<>" => {
                    if left_type == right_type {
                        Ok(ExprType::Bool)
                    } else {
                        Err(DbError::TypeError(format!(
                            "cannot compare {} with {} using '{}'",
                            left_type, right_type, op
                        )))
                    }
                }
                "<" | "<=" | ">" | ">=" => {
                    if left_type == right_type && left_type != ExprType::Bool {
                        Ok(ExprType::Bool)
                    } else {
                        Err(DbError::TypeError(format!(
                            "cannot order {} against {} using '{}'",
                            left_type, right_type, op
                        )))
                    }
                }
                "AND" | "OR" => {
                    if left_type == ExprType::Bool && right_type == ExprType::Bool {
                        Ok(ExprType::Bool)
                    } else {
                        Err(DbError::TypeError(format!(
                            "operator '{}' requires BOOL operands, got {} and {}",
                            op, left_type, right_type
                        )))
                    }
                }
                other => Err(DbError::Internal(format!(
                    "unknown binary operator '{}'",
                    other
                ))),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{Column, TableSchema};
    use crate::sql::parser::parse_statement;

    fn test_catalog() -> Catalog {
        let mut catalog = Catalog::new();
        catalog
            .add_table(TableSchema {
                name: "users".to_string(),
                columns: vec![
                    Column {
                        name: "id".to_string(),
                        data_type: DataType::Int,
                    },
                    Column {
                        name: "name".to_string(),
                        data_type: DataType::Varchar,
                    },
                ],
                root_page_id: 0,
                next_row_id: 1,
                indexes: vec![],
            })
            .unwrap();
        catalog
    }

    fn analyze_sql(sql: &str, catalog: &Catalog) -> Result<()> {
        let statement = parse_statement(sql).map_err(|e| DbError::Sql(e.to_string()))?;
        analyze(&statement, catalog)
    }

    #[test]
    fn test_valid_statements() {
        let catalog = test_catalog();
        analyze_sql("SELECT id, name FROM users WHERE id = 5;", &catalog).unwrap();
        analyze_sql("SELECT * FROM users ORDER BY name DESC;", &catalog).unwrap();
        analyze_sql("INSERT INTO users VALUES (1, 'alice');", &catalog).unwrap();
        analyze_sql("CREATE TABLE posts (id INT, title VARCHAR);", &catalog).unwrap();
        analyze_sql("SELECT * FROM users WHERE id + 1 < 10 AND name = 'bob';", &catalog).unwrap();
    }

    #[test]
    fn test_unknown_table() {
        let catalog = test_catalog();
        let result = analyze_sql("SELECT * FROM ghosts;", &catalog);
        assert!(matches!(result, Err(DbError::TableNotFound(_))));
    }

    #[test]
    fn test_unknown_column() {
        let catalog = test_catalog();
        let result = analyze_sql("SELECT age FROM users;", &catalog);
        assert!(matches!(result, Err(DbError::ColumnNotFound(..))));

        let result = analyze_sql("SELECT * FROM users WHERE age = 5;", &catalog);
        assert!(matches!(result, Err(DbError::ColumnNotFound(..))));

        let result = analyze_sql("SELECT * FROM users ORDER BY age;", &catalog);
        assert!(matches!(result, Err(DbError::ColumnNotFound(..))));
    }

    #[test]
    fn test_insert_arity_mismatch() {
        let catalog = test_catalog();
        let result = analyze_sql("INSERT INTO users VALUES (1);", &catalog);
        assert!(matches!(result, Err(DbError::Sql(_))));
    }

    #[test]
    fn test_insert_type_mismatch() {
        let catalog = test_catalog();
        let result = analyze_sql("INSERT INTO users VALUES ('one', 'alice');", &catalog);
        assert!(matches!(result, Err(DbError::TypeError(_))));
    }

    #[test]
    fn test_where_type_errors() {
        let catalog = test_catalog();

        // Comparing INT with VARCHAR
        let result = analyze_sql("SELECT * FROM users WHERE id = 'five';", &catalog);
        assert!(matches!(result, Err(DbError::TypeError(_))));

        // WHERE clause that is not boolean
        let result = analyze_sql("SELECT * FROM users WHERE id + 1;", &catalog);
        assert!(matches!(result, Err(DbError::TypeError(_))));

        // AND on non-boolean operands
        let result = analyze_sql("SELECT * FROM users WHERE id AND name;", &catalog);
        assert!(matches!(result, Err(DbError::TypeError(_))));
    }

    #[test]
    fn test_create_existing_table() {
        let catalog = test_catalog();
        let result = analyze_sql("CREATE TABLE users (id INT);", &catalog);
        assert!(matches!(result, Err(DbError::TableExists(_))));
    }

    #[test]
    fn test_create_duplicate_column() {
        let catalog = test_catalog();
        let result = analyze_sql("CREATE TABLE t (a INT, A VARCHAR);", &catalog);
        assert!(matches!(result, Err(DbError::Sql(_))));
    }
}
