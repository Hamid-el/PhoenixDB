use std::error::Error;
use std::fmt;

use crate::query::types::Value;
use crate::sql::ast::{DataType, Expression, Statement};
use crate::sql::lexer::{Lexer, LexerError, Token};

#[derive(Debug, Clone, PartialEq)]
pub enum ParseError {
    Lexer(LexerError),
    UnexpectedToken { expected: String, found: Token },
    UnexpectedEof { expected: String },
    Message(String),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::Lexer(err) => write!(f, "lexer error: {}", err),
            ParseError::UnexpectedToken { expected, found } => {
                write!(f, "expected {}, found {:?}", expected, found)
            }
            ParseError::UnexpectedEof { expected } => {
                write!(f, "expected {}, found end of input", expected)
            }
            ParseError::Message(message) => f.write_str(message),
        }
    }
}

impl Error for ParseError {}

impl From<LexerError> for ParseError {
    fn from(value: LexerError) -> Self {
        ParseError::Lexer(value)
    }
}

pub fn parse_statement(sql: &str) -> Result<Statement, ParseError> {
    Parser::parse(sql)
}

pub struct Parser {
    tokens: Vec<Token>,
    position: usize,
}

impl Parser {
    pub fn parse(sql: &str) -> Result<Statement, ParseError> {
        let mut lexer = Lexer::new(sql);
        let tokens = lexer.tokenize()?;
        Self::new(tokens).parse_statement()
    }

    pub fn new(tokens: Vec<Token>) -> Self {
        Self {
            tokens,
            position: 0,
        }
    }

    pub fn parse_statement(&mut self) -> Result<Statement, ParseError> {
        let statement = match self.peek() {
            Token::Create => self.parse_create_table()?,
            Token::Insert => self.parse_insert()?,
            Token::Select => self.parse_select()?,
            token => {
                return Err(ParseError::UnexpectedToken {
                    expected: "CREATE, INSERT, or SELECT".to_string(),
                    found: token.clone(),
                });
            }
        };

        self.consume_semicolon();
        self.expect_eof()?;
        Ok(statement)
    }

    fn parse_create_table(&mut self) -> Result<Statement, ParseError> {
        self.expect_token(Token::Create)?;
        self.expect_token(Token::Table)?;

        let name = self.expect_identifier()?;
        self.expect_token(Token::LParen)?;

        let mut columns = Vec::new();
        if !self.consume_token(&Token::RParen) {
            loop {
                let column_name = self.expect_identifier()?;
                let data_type = self.parse_data_type()?;
                columns.push((column_name, data_type));

                if self.consume_token(&Token::Comma) {
                    continue;
                }

                self.expect_token(Token::RParen)?;
                break;
            }
        }

        Ok(Statement::CreateTable { name, columns })
    }

    fn parse_insert(&mut self) -> Result<Statement, ParseError> {
        self.expect_token(Token::Insert)?;
        self.expect_token(Token::Into)?;

        let table_name = self.expect_identifier()?;
        self.expect_token(Token::Values)?;
        self.expect_token(Token::LParen)?;

        let mut values = Vec::new();
        if !self.consume_token(&Token::RParen) {
            loop {
                values.push(self.parse_value()?);

                if self.consume_token(&Token::Comma) {
                    continue;
                }

                self.expect_token(Token::RParen)?;
                break;
            }
        }

        Ok(Statement::Insert { table_name, values })
    }

    fn parse_select(&mut self) -> Result<Statement, ParseError> {
        self.expect_token(Token::Select)?;

        let projection = self.parse_projection()?;

        self.expect_token(Token::From)?;
        let table_name = self.expect_identifier()?;

        let where_clause = if self.consume_token(&Token::Where) {
            Some(self.parse_expression(0)?)
        } else {
            None
        };

        let order_by = if self.consume_token(&Token::Order) {
            self.expect_token(Token::By)?;
            let column = self.expect_identifier()?;
            let is_ascending = if self.consume_token(&Token::Desc) {
                false
            } else {
                self.consume_token(&Token::Asc);
                true
            };
            Some((column, is_ascending))
        } else {
            None
        };

        Ok(Statement::Select {
            projection,
            table_name,
            where_clause,
            order_by,
        })
    }

    fn parse_projection(&mut self) -> Result<Vec<String>, ParseError> {
        if self.consume_token(&Token::Asterisk) {
            return Ok(vec!["*".to_string()]);
        }

        let mut columns = Vec::new();
        loop {
            columns.push(self.expect_identifier()?);
            if !self.consume_token(&Token::Comma) {
                break;
            }
        }
        Ok(columns)
    }

    fn parse_data_type(&mut self) -> Result<DataType, ParseError> {
        match self.peek() {
            Token::Int => {
                self.advance();
                Ok(DataType::Int)
            }
            Token::Varchar => {
                self.advance();
                Ok(DataType::Varchar)
            }
            token => Err(ParseError::UnexpectedToken {
                expected: "INT or VARCHAR".to_string(),
                found: token.clone(),
            }),
        }
    }

    fn parse_value(&mut self) -> Result<Value, ParseError> {
        match self.peek() {
            Token::Number(n) => {
                let value = *n;
                self.advance();
                Ok(Value::Int(value))
            }
            Token::Minus => {
                self.advance();
                match self.peek() {
                    Token::Number(n) => {
                        let value = n.checked_neg().ok_or_else(|| {
                            ParseError::Message("integer literal is out of range".to_string())
                        })?;
                        self.advance();
                        Ok(Value::Int(value))
                    }
                    token => Err(ParseError::UnexpectedToken {
                        expected: "number after '-'".to_string(),
                        found: token.clone(),
                    }),
                }
            }
            Token::String(s) => {
                let value = s.clone();
                self.advance();
                Ok(Value::Varchar(value))
            }
            Token::True => {
                self.advance();
                Ok(Value::Bool(true))
            }
            Token::False => {
                self.advance();
                Ok(Value::Bool(false))
            }
            token => Err(ParseError::UnexpectedToken {
                expected: "literal value".to_string(),
                found: token.clone(),
            }),
        }
    }

    fn parse_expression(&mut self, min_binding_power: u8) -> Result<Expression, ParseError> {
        let mut left = self.parse_prefix_expression()?;

        loop {
            let Some((left_bp, right_bp, op)) = infix_binding_power(self.peek()) else {
                break;
            };

            if left_bp < min_binding_power {
                break;
            }

            self.advance();
            let right = self.parse_expression(right_bp)?;
            left = Expression::BinaryOp {
                left: Box::new(left),
                op: op.to_string(),
                right: Box::new(right),
            };
        }

        Ok(left)
    }

    fn parse_prefix_expression(&mut self) -> Result<Expression, ParseError> {
        match self.peek() {
            Token::Identifier(name) => {
                let ident = name.clone();
                self.advance();
                Ok(Expression::Identifier(ident))
            }
            Token::Number(_) | Token::Minus | Token::String(_) | Token::True | Token::False => {
                Ok(Expression::Literal(self.parse_value()?))
            }
            Token::LParen => {
                self.advance();
                let expression = self.parse_expression(0)?;
                self.expect_token(Token::RParen)?;
                Ok(expression)
            }
            token => Err(ParseError::UnexpectedToken {
                expected: "expression".to_string(),
                found: token.clone(),
            }),
        }
    }

    fn expect_identifier(&mut self) -> Result<String, ParseError> {
        match self.peek() {
            Token::Identifier(name) => {
                let ident = name.clone();
                self.advance();
                Ok(ident)
            }
            token => Err(ParseError::UnexpectedToken {
                expected: "identifier".to_string(),
                found: token.clone(),
            }),
        }
    }

    fn expect_token(&mut self, expected: Token) -> Result<(), ParseError> {
        let found = self.peek().clone();
        if found == expected {
            self.advance();
            Ok(())
        } else if found == Token::Eof {
            Err(ParseError::UnexpectedEof {
                expected: format!("{:?}", expected),
            })
        } else {
            Err(ParseError::UnexpectedToken {
                expected: format!("{:?}", expected),
                found,
            })
        }
    }

    fn expect_eof(&self) -> Result<(), ParseError> {
        match self.peek() {
            Token::Eof => Ok(()),
            token => Err(ParseError::UnexpectedToken {
                expected: "end of input".to_string(),
                found: token.clone(),
            }),
        }
    }

    fn consume_token(&mut self, token: &Token) -> bool {
        if self.peek() == token {
            self.advance();
            true
        } else {
            false
        }
    }

    fn consume_semicolon(&mut self) {
        self.consume_token(&Token::Semicolon);
    }

    fn peek(&self) -> &Token {
        self.tokens.get(self.position).unwrap_or(&Token::Eof)
    }

    fn advance(&mut self) {
        if self.position < self.tokens.len() {
            self.position += 1;
        }
    }
}

fn infix_binding_power(token: &Token) -> Option<(u8, u8, &'static str)> {
    match token {
        Token::Or => Some((1, 2, "OR")),
        Token::And => Some((3, 4, "AND")),
        Token::Eq => Some((5, 6, "=")),
        Token::NotEq => Some((5, 6, "<>")),
        Token::Lt => Some((5, 6, "<")),
        Token::Lte => Some((5, 6, "<=")),
        Token::Gt => Some((5, 6, ">")),
        Token::Gte => Some((5, 6, ">=")),
        Token::Plus => Some((7, 8, "+")),
        Token::Minus => Some((7, 8, "-")),
        Token::Asterisk => Some((9, 10, "*")),
        Token::Slash => Some((9, 10, "/")),
        _ => None,
    }
}
