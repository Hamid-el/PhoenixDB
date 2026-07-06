use std::error::Error;
use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Create,
    Table,
    Int,
    Varchar,
    Insert,
    Into,
    Values,
    Select,
    From,
    Where,
    Order,
    By,
    Asc,
    Desc,
    True,
    False,
    And,
    Or,
    Begin,
    Commit,
    Rollback,
    Transaction,
    Identifier(String),
    Number(i32),
    String(String),
    Eq,
    NotEq,
    Lt,
    Lte,
    Gt,
    Gte,
    Plus,
    Minus,
    Slash,
    Asterisk,
    LParen,
    RParen,
    Comma,
    Semicolon,
    Eof,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LexerError {
    UnexpectedCharacter { ch: char, position: usize },
    UnterminatedString { position: usize },
    InvalidNumber { value: String, position: usize },
}

impl fmt::Display for LexerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LexerError::UnexpectedCharacter { ch, position } => {
                write!(f, "unexpected character '{}' at position {}", ch, position)
            }
            LexerError::UnterminatedString { position } => {
                write!(f, "unterminated string starting at position {}", position)
            }
            LexerError::InvalidNumber { value, position } => {
                write!(f, "invalid number '{}' at position {}", value, position)
            }
        }
    }
}

impl Error for LexerError {}

pub struct Lexer<'a> {
    input: &'a str,
    chars: Vec<char>,
    position: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(input: &'a str) -> Self {
        Self {
            input,
            chars: input.chars().collect(),
            position: 0,
        }
    }

    pub fn tokenize(&mut self) -> Result<Vec<Token>, LexerError> {
        let mut tokens = Vec::new();

        loop {
            self.skip_whitespace();

            let Some(ch) = self.peek_char() else {
                tokens.push(Token::Eof);
                return Ok(tokens);
            };

            let token = match ch {
                '(' => {
                    self.advance();
                    Token::LParen
                }
                ')' => {
                    self.advance();
                    Token::RParen
                }
                ',' => {
                    self.advance();
                    Token::Comma
                }
                ';' => {
                    self.advance();
                    Token::Semicolon
                }
                '*' => {
                    self.advance();
                    Token::Asterisk
                }
                '+' => {
                    self.advance();
                    Token::Plus
                }
                '-' => {
                    self.advance();
                    Token::Minus
                }
                '/' => {
                    self.advance();
                    Token::Slash
                }
                '=' => {
                    self.advance();
                    Token::Eq
                }
                '<' => self.lex_less_than(),
                '>' => self.lex_greater_than(),
                '!' => self.lex_bang()?,
                '\'' => self.lex_string()?,
                ch if ch.is_ascii_digit() => self.lex_number()?,
                ch if is_identifier_start(ch) => self.lex_identifier(),
                ch => {
                    return Err(LexerError::UnexpectedCharacter {
                        ch,
                        position: self.position,
                    });
                }
            };

            tokens.push(token);
        }
    }

    pub fn input(&self) -> &str {
        self.input
    }

    fn skip_whitespace(&mut self) {
        while self.peek_char().is_some_and(|ch| ch.is_whitespace()) {
            self.advance();
        }
    }

    fn lex_identifier(&mut self) -> Token {
        let start = self.position;
        while self.peek_char().is_some_and(is_identifier_part) {
            self.advance();
        }

        let ident: String = self.chars[start..self.position].iter().collect();
        match ident.to_ascii_uppercase().as_str() {
            "CREATE" => Token::Create,
            "TABLE" => Token::Table,
            "INT" => Token::Int,
            "VARCHAR" => Token::Varchar,
            "INSERT" => Token::Insert,
            "INTO" => Token::Into,
            "VALUES" => Token::Values,
            "SELECT" => Token::Select,
            "FROM" => Token::From,
            "WHERE" => Token::Where,
            "ORDER" => Token::Order,
            "BY" => Token::By,
            "ASC" => Token::Asc,
            "DESC" => Token::Desc,
            "TRUE" => Token::True,
            "FALSE" => Token::False,
            "AND" => Token::And,
            "OR" => Token::Or,
            "BEGIN" => Token::Begin,
            "COMMIT" => Token::Commit,
            "ROLLBACK" => Token::Rollback,
            "TRANSACTION" => Token::Transaction,
            _ => Token::Identifier(ident),
        }
    }

    fn lex_number(&mut self) -> Result<Token, LexerError> {
        let start = self.position;
        while self.peek_char().is_some_and(|ch| ch.is_ascii_digit()) {
            self.advance();
        }

        let value: String = self.chars[start..self.position].iter().collect();
        value
            .parse::<i32>()
            .map(Token::Number)
            .map_err(|_| LexerError::InvalidNumber {
                value,
                position: start,
            })
    }

    fn lex_string(&mut self) -> Result<Token, LexerError> {
        let start = self.position;
        self.advance();

        let mut value = String::new();
        loop {
            match self.peek_char() {
                Some('\'') => {
                    self.advance();
                    if self.peek_char() == Some('\'') {
                        value.push('\'');
                        self.advance();
                    } else {
                        return Ok(Token::String(value));
                    }
                }
                Some(ch) => {
                    value.push(ch);
                    self.advance();
                }
                None => return Err(LexerError::UnterminatedString { position: start }),
            }
        }
    }

    fn lex_less_than(&mut self) -> Token {
        self.advance();
        match self.peek_char() {
            Some('=') => {
                self.advance();
                Token::Lte
            }
            Some('>') => {
                self.advance();
                Token::NotEq
            }
            _ => Token::Lt,
        }
    }

    fn lex_bang(&mut self) -> Result<Token, LexerError> {
        let start = self.position;
        self.advance();
        if self.peek_char() == Some('=') {
            self.advance();
            Ok(Token::NotEq)
        } else {
            Err(LexerError::UnexpectedCharacter {
                ch: '!',
                position: start,
            })
        }
    }

    fn lex_greater_than(&mut self) -> Token {
        self.advance();
        if self.peek_char() == Some('=') {
            self.advance();
            Token::Gte
        } else {
            Token::Gt
        }
    }

    fn peek_char(&self) -> Option<char> {
        self.chars.get(self.position).copied()
    }

    fn advance(&mut self) {
        self.position += 1;
    }
}

fn is_identifier_start(ch: char) -> bool {
    ch.is_ascii_alphabetic() || ch == '_'
}

fn is_identifier_part(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}
