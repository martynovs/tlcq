//! Just enough HTTP/1.1 for the runner's page (design D16): one request per
//! connection, answered and closed, one connection at a time. The page sits
//! on the home network behind pve's traefik, with no login, and serves one
//! human; nothing here needs more.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

/// The longest request line or header line read.
const LINE_LIMIT: u64 = 8 * 1024;

/// The largest body read.
const BODY_LIMIT: usize = 64 * 1024;

/// A request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Request {
    /// `GET` or `POST`.
    pub method: String,
    /// The path, without its query.
    pub path: String,
    /// The query's pairs, as given (no percent-decoding: the page asks only for
    /// ids, run names and numbers).
    pub query: Vec<(String, String)>,
    /// The body.
    pub body: Vec<u8>,
    /// The body's content type, as given.
    pub content_type: Option<String>,
}

impl Request {
    /// The query value of `name`.
    pub fn query(&self, name: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// A response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// The status code.
    pub status: u16,
    /// The body's content type.
    pub content_type: &'static str,
    /// The body.
    pub body: Vec<u8>,
}

impl Response {
    /// A 200 with `body`.
    pub fn ok(content_type: &'static str, body: Vec<u8>) -> Self {
        Self {
            status: 200,
            content_type,
            body,
        }
    }

    /// An error `status`, saying why.
    pub fn error(status: u16, why: &str) -> Self {
        Self {
            status,
            content_type: "text/plain; charset=utf-8",
            body: why.as_bytes().to_vec(),
        }
    }

    /// Write the response, then the connection closes.
    ///
    /// # Errors
    /// If it cannot be written.
    pub fn write(&self, out: &mut impl Write) -> std::io::Result<()> {
        let reason = match self.status {
            200 => "OK",
            400 => "Bad Request",
            404 => "Not Found",
            415 => "Unsupported Media Type",
            _ => "Internal Server Error",
        };
        write!(
            out,
            "HTTP/1.1 {} {reason}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
            self.status,
            self.content_type,
            self.body.len()
        )?;
        out.write_all(&self.body)?;
        out.flush()
    }
}

/// Read one request.
///
/// # Errors
/// If it cannot be read, or is not a request this server reads.
pub fn read_request(input: &mut impl BufRead) -> std::io::Result<Request> {
    let bad = |why: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, why.to_owned());
    let first = line(input)?;
    let mut words = first.split(' ');
    let (Some(method), Some(target)) = (words.next(), words.next()) else {
        return Err(bad("no request line"));
    };
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let mut length = 0;
    let mut content_type = None;
    loop {
        let header = line(input)?;
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                length = value
                    .trim()
                    .parse()
                    .map_err(|e| bad(&format!("a bad length: {e}")))?;
            } else if name.eq_ignore_ascii_case("content-type") {
                content_type = Some(value.trim().to_owned());
            }
        }
    }
    if length > BODY_LIMIT {
        return Err(bad("the body is too long"));
    }
    let mut body = vec![0; length];
    input.read_exact(&mut body)?;
    Ok(Request {
        method: method.to_owned(),
        path: path.to_owned(),
        query: query
            .split('&')
            .filter(|p| !p.is_empty())
            .map(|p| {
                let (k, v) = p.split_once('=').unwrap_or((p, ""));
                (k.to_owned(), v.to_owned())
            })
            .collect(),
        body,
        content_type,
    })
}

/// One line, without its line end; at most [`LINE_LIMIT`] bytes.
fn line(input: &mut impl BufRead) -> std::io::Result<String> {
    let mut bytes = Vec::new();
    input.take(LINE_LIMIT).read_until(b'\n', &mut bytes)?;
    if bytes.last() != Some(&b'\n') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "a line cut short",
        ));
    }
    let text = String::from_utf8_lossy(&bytes);
    Ok(text.trim_end_matches(['\r', '\n']).to_owned())
}

/// Answer each connection to `listener` with `handle`, for ever. A client
/// that sends nothing for 10 seconds is dropped.
pub fn serve(listener: &TcpListener, mut handle: impl FnMut(&Request) -> Response) {
    for stream in listener.incoming().flatten() {
        if let Err(e) = answer(&stream, &mut handle) {
            eprintln!("tlcq runner: the page: {e}");
        }
    }
}

/// Answer one connection.
fn answer(
    stream: &TcpStream,
    handle: &mut impl FnMut(&Request) -> Response,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut input = BufReader::new(stream.try_clone()?);
    let response = match read_request(&mut input) {
        Ok(req) => handle(&req),
        Err(e) => Response::error(400, &e.to_string()),
    };
    response.write(&mut &*stream)
}

#[cfg(test)]
#[path = "http_tests.rs"]
mod tests;
