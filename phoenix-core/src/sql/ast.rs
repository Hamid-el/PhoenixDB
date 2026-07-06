use crate::query::types::Value;

#[derive(Debug, Clone, PartialEq)]
pub enum DataType {
    Int,
    Varchar,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expression {
    Literal(Value),
    Identifier(String),
    BinaryOp {
        left: Box<Expression>,
        op: String,
        right: Box<Expression>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Statement {
    CreateTable {
        name: String,
        columns: Vec<(String, DataType)>,
    },
    Insert {
        table_name: String,
        values: Vec<Value>,
    },
    Select {
        projection: Vec<String>,
        table_name: String,
        where_clause: Option<Expression>,
        order_by: Option<(String, bool)>,
    },
    Begin,
    Commit,
    Rollback,
}
