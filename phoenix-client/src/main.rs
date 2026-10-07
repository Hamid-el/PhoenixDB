use std::io::{self, BufRead, BufReader, Write};
use std::net::TcpStream;

const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: u16 = 7878;

fn main() {
    env_logger::init();

    let (host, port) = match parse_args() {
        Ok(pair) => pair,
        Err(message) => {
            eprintln!("{}", message);
            eprintln!("usage: phoenix-client [--host <host>] [--port <port>]");
            std::process::exit(2);
        }
    };

    let address = format!("{}:{}", host, port);
    let stream = match TcpStream::connect(&address) {
        Ok(stream) => stream,
        Err(e) => {
            eprintln!("failed to connect to {}: {}", address, e);
            std::process::exit(1);
        }
    };

    println!("Connected to PhoenixDB at {}", address);
    println!("Type SQL statements terminated by Enter. 'exit' or 'quit' to leave.");

    let mut server_reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let mut server_writer = stream;

    let stdin = io::stdin();
    loop {
        print!("phoenix> ");
        io::stdout().flush().ok();

        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(e) => {
                eprintln!("stdin error: {}", e);
                break;
            }
        }

        let sql = line.trim();
        if sql.is_empty() {
            continue;
        }
        if sql.eq_ignore_ascii_case("exit") || sql.eq_ignore_ascii_case("quit") {
            break;
        }

        if server_writer
            .write_all(format!("{}\n", sql).as_bytes())
            .is_err()
        {
            eprintln!("connection lost");
            break;
        }

        match read_reply(&mut server_reader) {
            Ok(reply) => print_reply(&reply),
            Err(e) => {
                eprintln!("connection lost: {}", e);
                break;
            }
        }
    }

    println!("bye");
}

fn parse_args() -> Result<(String, u16), String> {
    let mut host = DEFAULT_HOST.to_string();
    let mut port = DEFAULT_PORT;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--host" => host = args.next().ok_or("--host needs a value")?,
            "--port" => {
                let value = args.next().ok_or("--port needs a value")?;
                port = value
                    .parse()
                    .map_err(|_| format!("invalid port '{}'", value))?;
            }
            other => return Err(format!("unknown argument '{}'", other)),
        }
    }
    Ok((host, port))
}

/// A parsed RESP reply.
enum Reply {
    Simple(String),
    Error(String),
    Integer(i64),
    Bulk(String),
    Array(Vec<Reply>),
}

fn read_reply(reader: &mut impl BufRead) -> io::Result<Reply> {
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "server closed the connection",
        ));
    }
    let line = line.trim_end_matches(['\r', '\n']);
    let (prefix, rest) = line.split_at(1);

    match prefix {
        "+" => Ok(Reply::Simple(rest.to_string())),
        "-" => Ok(Reply::Error(rest.to_string())),
        ":" => Ok(Reply::Integer(rest.parse().map_err(invalid_data)?)),
        "$" => {
            let len: usize = rest.parse().map_err(invalid_data)?;
            let mut buf = vec![0u8; len + 2]; // payload + CRLF
            reader.read_exact(&mut buf)?;
            buf.truncate(len);
            String::from_utf8(buf)
                .map(Reply::Bulk)
                .map_err(invalid_data)
        }
        "*" => {
            let count: usize = rest.parse().map_err(invalid_data)?;
            let mut items = Vec::with_capacity(count);
            for _ in 0..count {
                items.push(read_reply(reader)?);
            }
            Ok(Reply::Array(items))
        }
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unknown RESP prefix '{}'", other),
        )),
    }
}

fn invalid_data(e: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}

fn print_reply(reply: &Reply) {
    match reply {
        Reply::Simple(s) => println!("{}", s),
        Reply::Error(message) => println!("(error) {}", message),
        Reply::Integer(n) => println!("({} row{} affected)", n, if *n == 1 { "" } else { "s" }),
        Reply::Bulk(s) => println!("{}", s),
        Reply::Array(items) => print_table(items),
    }
}

/// SELECT results arrive as an array of rows, the first row being the
/// column header. Renders them as an aligned text table.
fn print_table(items: &[Reply]) {
    let rows: Vec<Vec<String>> = items
        .iter()
        .map(|item| match item {
            Reply::Array(fields) => fields
                .iter()
                .map(|f| match f {
                    Reply::Bulk(s) => s.clone(),
                    Reply::Simple(s) => s.clone(),
                    Reply::Integer(n) => n.to_string(),
                    _ => String::from("?"),
                })
                .collect(),
            Reply::Bulk(s) => vec![s.clone()],
            _ => vec![String::from("?")],
        })
        .collect();

    let Some((header, data)) = rows.split_first() else {
        println!("(empty result)");
        return;
    };

    let num_columns = header.len();
    let mut widths: Vec<usize> = header.iter().map(|h| h.len()).collect();
    for row in data {
        for (i, field) in row.iter().enumerate().take(num_columns) {
            widths[i] = widths[i].max(field.len());
        }
    }

    let print_row = |row: &[String]| {
        let cells: Vec<String> = row
            .iter()
            .enumerate()
            .map(|(i, field)| format!("{:<width$}", field, width = widths.get(i).copied().unwrap_or(0)))
            .collect();
        println!("| {} |", cells.join(" | "));
    };

    let separator: Vec<String> = widths.iter().map(|w| "-".repeat(*w)).collect();
    print_row(header);
    println!("|-{}-|", separator.join("-|-"));
    for row in data {
        print_row(row);
    }
    println!(
        "({} row{})",
        data.len(),
        if data.len() == 1 { "" } else { "s" }
    );
}
