use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;

use log::{error, info, warn};

use phoenix_core::engine::{Engine, QueryResult, Session};

const DEFAULT_PORT: u16 = 7878;
const DEFAULT_DB_PATH: &str = "phoenix.db";

fn main() {
    env_logger::init();

    let config = match Config::from_args() {
        Ok(config) => config,
        Err(message) => {
            eprintln!("{}", message);
            eprintln!("usage: phoenix-server [--port <port>] [--db <path>]");
            std::process::exit(2);
        }
    };

    let engine = match Engine::open(&config.db_path) {
        Ok(engine) => Arc::new(engine),
        Err(e) => {
            eprintln!("failed to open database '{}': {}", config.db_path, e);
            std::process::exit(1);
        }
    };

    let address = format!("127.0.0.1:{}", config.port);
    let listener = match TcpListener::bind(&address) {
        Ok(listener) => listener,
        Err(e) => {
            eprintln!("failed to bind {}: {}", address, e);
            std::process::exit(1);
        }
    };

    println!("PhoenixDB server listening on {} (db: {})", address, config.db_path);
    info!("PhoenixDB server listening on {}", address);

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let engine = Arc::clone(&engine);
                thread::spawn(move || handle_connection(stream, engine));
            }
            Err(e) => warn!("failed to accept connection: {}", e),
        }
    }
}

struct Config {
    port: u16,
    db_path: String,
}

impl Config {
    fn from_args() -> Result<Self, String> {
        let mut config = Config {
            port: DEFAULT_PORT,
            db_path: DEFAULT_DB_PATH.to_string(),
        };

        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--port" => {
                    let value = args.next().ok_or("--port needs a value")?;
                    config.port = value
                        .parse()
                        .map_err(|_| format!("invalid port '{}'", value))?;
                }
                "--db" => {
                    config.db_path = args.next().ok_or("--db needs a value")?;
                }
                other => return Err(format!("unknown argument '{}'", other)),
            }
        }
        Ok(config)
    }
}

fn handle_connection(stream: TcpStream, engine: Arc<Engine>) {
    let peer = stream
        .peer_addr()
        .map(|a| a.to_string())
        .unwrap_or_else(|_| "<unknown>".to_string());
    info!("connection from {}", peer);

    // Dropping the session on disconnect discards any open transaction
    // (implicit rollback).
    let mut session = Session::new();

    let reader = match stream.try_clone() {
        Ok(clone) => BufReader::new(clone),
        Err(e) => {
            error!("failed to clone stream for {}: {}", peer, e);
            return;
        }
    };
    let mut writer = stream;

    for line in reader.lines() {
        let line = match line {
            Ok(line) => line,
            Err(_) => break, // client went away
        };
        let sql = line.trim();
        if sql.is_empty() {
            continue;
        }

        let response = match engine.execute(sql, &mut session) {
            Ok(result) => encode_result(&result),
            Err(e) => encode_error(&e.to_string()),
        };

        if writer.write_all(&response).is_err() || writer.flush().is_err() {
            break;
        }
    }

    info!("connection from {} closed", peer);
}

fn encode_result(result: &QueryResult) -> Vec<u8> {
    let mut out = Vec::new();
    match result {
        QueryResult::Ok => out.extend_from_slice(b"+OK\r\n"),
        QueryResult::Inserted(n) => {
            out.extend_from_slice(format!(":{}\r\n", n).as_bytes());
        }
        QueryResult::Rows { columns, rows } => {
            out.extend_from_slice(format!("*{}\r\n", rows.len() + 1).as_bytes());
            encode_string_array(&mut out, columns.iter().map(|c| c.as_str()));
            for row in rows {
                let fields: Vec<String> = row.iter().map(|v| v.to_string()).collect();
                encode_string_array(&mut out, fields.iter().map(|f| f.as_str()));
            }
        }
    }
    out
}

fn encode_string_array<'a>(out: &mut Vec<u8>, items: impl ExactSizeIterator<Item = &'a str>) {
    out.extend_from_slice(format!("*{}\r\n", items.len()).as_bytes());
    for item in items {
        out.extend_from_slice(format!("${}\r\n", item.len()).as_bytes());
        out.extend_from_slice(item.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
}

fn encode_error(message: &str) -> Vec<u8> {
    // RESP error messages are single-line.
    let clean: String = message
        .chars()
        .map(|c| if c == '\r' || c == '\n' { ' ' } else { c })
        .collect();
    format!("-ERR {}\r\n", clean).into_bytes()
}
