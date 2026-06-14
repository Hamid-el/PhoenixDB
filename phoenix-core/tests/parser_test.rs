use phoenix_core::query::types::Value;
use phoenix_core::sql::{parse_statement, DataType, Expression, Parser, Statement};

#[test]
fn test_parse_create_table() {
    let statement = parse_statement("CREATE TABLE users (id INT, name VARCHAR);").unwrap();

    assert_eq!(
        statement,
        Statement::CreateTable {
            name: "users".to_string(),
            columns: vec![
                ("id".to_string(), DataType::Int),
                ("name".to_string(), DataType::Varchar),
            ],
        }
    );
}

#[test]
fn test_parse_insert_values() {
    let statement = Parser::parse("INSERT INTO users VALUES (1, 'Alice', true);").unwrap();

    assert_eq!(
        statement,
        Statement::Insert {
            table_name: "users".to_string(),
            values: vec![
                Value::Int(1),
                Value::Varchar("Alice".to_string()),
                Value::Bool(true),
            ],
        }
    );
}

#[test]
fn test_parse_select_star_where_order_desc() {
    let statement = Parser::parse("SELECT * FROM users WHERE id = 5 ORDER BY name DESC;").unwrap();

    assert_eq!(
        statement,
        Statement::Select {
            projection: vec!["*".to_string()],
            table_name: "users".to_string(),
            where_clause: Some(Expression::BinaryOp {
                left: Box::new(Expression::Identifier("id".to_string())),
                op: "=".to_string(),
                right: Box::new(Expression::Literal(Value::Int(5))),
            }),
            order_by: Some(("name".to_string(), false)),
        }
    );
}

#[test]
fn test_parse_select_columns_default_order_ascending() {
    let statement = Parser::parse("select id, name from users order by id;").unwrap();

    assert_eq!(
        statement,
        Statement::Select {
            projection: vec!["id".to_string(), "name".to_string()],
            table_name: "users".to_string(),
            where_clause: None,
            order_by: Some(("id".to_string(), true)),
        }
    );
}

#[test]
fn test_parse_pratt_expression_precedence() {
    let statement =
        Parser::parse("SELECT id FROM users WHERE active = true OR age > 18 AND name <> 'guest';")
            .unwrap();

    let Statement::Select {
        where_clause: Some(expression),
        ..
    } = statement
    else {
        panic!("expected SELECT with WHERE clause");
    };

    assert_eq!(
        expression,
        Expression::BinaryOp {
            left: Box::new(Expression::BinaryOp {
                left: Box::new(Expression::Identifier("active".to_string())),
                op: "=".to_string(),
                right: Box::new(Expression::Literal(Value::Bool(true))),
            }),
            op: "OR".to_string(),
            right: Box::new(Expression::BinaryOp {
                left: Box::new(Expression::BinaryOp {
                    left: Box::new(Expression::Identifier("age".to_string())),
                    op: ">".to_string(),
                    right: Box::new(Expression::Literal(Value::Int(18))),
                }),
                op: "AND".to_string(),
                right: Box::new(Expression::BinaryOp {
                    left: Box::new(Expression::Identifier("name".to_string())),
                    op: "<>".to_string(),
                    right: Box::new(Expression::Literal(Value::Varchar("guest".to_string()))),
                }),
            }),
        }
    );
}

#[test]
fn test_parse_rejects_trailing_tokens() {
    let err = Parser::parse("SELECT * FROM users; SELECT * FROM orders;").unwrap_err();
    assert!(format!("{}", err).contains("end of input"));
}
