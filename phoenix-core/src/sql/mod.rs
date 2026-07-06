pub mod analyzer;
pub mod ast;
pub mod lexer;
pub mod optimizer;
pub mod parser;

pub use analyzer::{analyze, type_check_expression, ExprType};
pub use ast::{DataType, Expression, Statement};
pub use lexer::{Lexer, LexerError, Token};
pub use optimizer::{fold_expression, optimize};
pub use parser::{parse_statement, ParseError, Parser};
