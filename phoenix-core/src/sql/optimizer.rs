use crate::query::types::Value;
use crate::sql::ast::{Expression, Statement};

pub fn optimize(statement: Statement) -> Statement {
    match statement {
        Statement::Select {
            projection,
            table_name,
            where_clause,
            order_by,
        } => {
            let where_clause = where_clause.map(fold_expression).and_then(|expr| {
                match expr {
                    // WHERE TRUE (e.g. from `WHERE 1 = 1`) filters nothing: drop it.
                    Expression::Literal(Value::Bool(true)) => None,
                    other => Some(other),
                }
            });
            Statement::Select {
                projection,
                table_name,
                where_clause,
                order_by,
            }
        }
        other => other,
    }
}

/// Recursively folds constant sub-expressions bottom-up.
pub fn fold_expression(expression: Expression) -> Expression {
    let Expression::BinaryOp { left, op, right } = expression else {
        return expression;
    };

    let left = fold_expression(*left);
    let right = fold_expression(*right);

    if let (Expression::Literal(l), Expression::Literal(r)) = (&left, &right) {
        if let Some(folded) = eval_const(l, &op, r) {
            return Expression::Literal(folded);
        }
    }

    // Boolean identities. Safe to drop the non-literal side because
    // expressions have no side effects and evaluation cannot produce NULL.
    match op.as_str() {
        "AND" => match (&left, &right) {
            (Expression::Literal(Value::Bool(true)), _) => return right,
            (_, Expression::Literal(Value::Bool(true))) => return left,
            (Expression::Literal(Value::Bool(false)), _)
            | (_, Expression::Literal(Value::Bool(false))) => {
                return Expression::Literal(Value::Bool(false));
            }
            _ => {}
        },
        "OR" => match (&left, &right) {
            (Expression::Literal(Value::Bool(false)), _) => return right,
            (_, Expression::Literal(Value::Bool(false))) => return left,
            (Expression::Literal(Value::Bool(true)), _)
            | (_, Expression::Literal(Value::Bool(true))) => {
                return Expression::Literal(Value::Bool(true));
            }
            _ => {}
        },
        _ => {}
    }

    Expression::BinaryOp {
        left: Box::new(left),
        op,
        right: Box::new(right),
    }
}

/// Evaluates a binary operation on two literals. Returns `None` when the
/// operation cannot be safely folded (type mismatch, overflow, division by
/// zero) — those cases are left for the analyzer/executor to report.
fn eval_const(left: &Value, op: &str, right: &Value) -> Option<Value> {
    match op {
        "+" | "-" | "*" | "/" => {
            let (Value::Int(a), Value::Int(b)) = (left, right) else {
                return None;
            };
            let result = match op {
                "+" => a.checked_add(*b),
                "-" => a.checked_sub(*b),
                "*" => a.checked_mul(*b),
                "/" => a.checked_div(*b),
                _ => unreachable!(),
            };
            result.map(Value::Int)
        }
        "=" | "<>" | "<" | "<=" | ">" | ">=" => {
            // Only compare like with like; mixed types are a type error the
            // analyzer reports.
            let comparable = matches!(
                (left, right),
                (Value::Int(_), Value::Int(_))
                    | (Value::Varchar(_), Value::Varchar(_))
                    | (Value::Bool(_), Value::Bool(_))
            );
            if !comparable || (matches!(left, Value::Bool(_)) && !matches!(op, "=" | "<>")) {
                return None;
            }
            let result = match op {
                "=" => left == right,
                "<>" => left != right,
                "<" => left < right,
                "<=" => left <= right,
                ">" => left > right,
                ">=" => left >= right,
                _ => unreachable!(),
            };
            Some(Value::Bool(result))
        }
        "AND" => match (left, right) {
            (Value::Bool(a), Value::Bool(b)) => Some(Value::Bool(*a && *b)),
            _ => None,
        },
        "OR" => match (left, right) {
            (Value::Bool(a), Value::Bool(b)) => Some(Value::Bool(*a || *b)),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::parser::parse_statement;

    fn optimized_where(sql: &str) -> Option<Expression> {
        let statement = parse_statement(sql).unwrap();
        match optimize(statement) {
            Statement::Select { where_clause, .. } => where_clause,
            _ => panic!("expected SELECT"),
        }
    }

    fn literal_int(n: i32) -> Expression {
        Expression::Literal(Value::Int(n))
    }

    #[test]
    fn test_constant_folding_arithmetic() {
        let expr = optimized_where("SELECT * FROM t WHERE id = 5 + 3;").unwrap();
        assert_eq!(
            expr,
            Expression::BinaryOp {
                left: Box::new(Expression::Identifier("id".to_string())),
                op: "=".to_string(),
                right: Box::new(literal_int(8)),
            }
        );
    }

    #[test]
    fn test_nested_folding() {
        let expr = optimized_where("SELECT * FROM t WHERE id = (2 + 3) * 4 - 6 / 2;").unwrap();
        assert_eq!(
            expr,
            Expression::BinaryOp {
                left: Box::new(Expression::Identifier("id".to_string())),
                op: "=".to_string(),
                right: Box::new(literal_int(17)),
            }
        );
    }

    #[test]
    fn test_where_one_equals_one_dropped() {
        assert_eq!(optimized_where("SELECT * FROM t WHERE 1 = 1;"), None);
        assert_eq!(optimized_where("SELECT * FROM t WHERE TRUE;"), None);
    }

    #[test]
    fn test_where_tautology_and_condition() {
        // `1 = 1 AND id > 5` simplifies to `id > 5`
        let expr = optimized_where("SELECT * FROM t WHERE 1 = 1 AND id > 5;").unwrap();
        assert_eq!(
            expr,
            Expression::BinaryOp {
                left: Box::new(Expression::Identifier("id".to_string())),
                op: ">".to_string(),
                right: Box::new(literal_int(5)),
            }
        );
    }

    #[test]
    fn test_where_contradiction_folds_to_false() {
        let expr = optimized_where("SELECT * FROM t WHERE 1 = 2 AND id > 5;").unwrap();
        assert_eq!(expr, Expression::Literal(Value::Bool(false)));
    }

    #[test]
    fn test_or_with_true_folds_to_true_and_is_dropped() {
        assert_eq!(
            optimized_where("SELECT * FROM t WHERE id > 5 OR 1 = 1;"),
            None
        );
    }

    #[test]
    fn test_string_comparison_folds() {
        let expr = optimized_where("SELECT * FROM t WHERE 'a' < 'b';");
        assert_eq!(expr, None); // folds to TRUE, gets dropped
    }

    #[test]
    fn test_division_by_zero_not_folded() {
        let expr = optimized_where("SELECT * FROM t WHERE id = 1 / 0;").unwrap();
        assert_eq!(
            expr,
            Expression::BinaryOp {
                left: Box::new(Expression::Identifier("id".to_string())),
                op: "=".to_string(),
                right: Box::new(Expression::BinaryOp {
                    left: Box::new(literal_int(1)),
                    op: "/".to_string(),
                    right: Box::new(literal_int(0)),
                }),
            }
        );
    }

    #[test]
    fn test_overflow_not_folded() {
        let expr = optimized_where("SELECT * FROM t WHERE id = 2147483647 + 1;").unwrap();
        assert!(matches!(expr, Expression::BinaryOp { .. }));
    }
}
