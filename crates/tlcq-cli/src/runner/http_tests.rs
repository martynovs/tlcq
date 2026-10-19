//! Tests of the page's HTTP: reading requests, writing responses, serving.

use std::io::{Read, Write};

use super::*;

fn read(text: &str) -> std::io::Result<Request> {
    read_request(&mut text.as_bytes())
}

#[test]
fn a_request_gives_its_method_path_query_and_body() {
    let get = read("GET /api/log?id=ab&run=v1&attempt=2&flag HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
    assert_eq!(
        (get.method.as_str(), get.path.as_str()),
        ("GET", "/api/log")
    );
    assert_eq!(
        (get.query("run"), get.query("flag"), get.query("no")),
        (Some("v1"), Some(""), None)
    );
    let post = read("POST /api/cancel HTTP/1.1\r\ncontent-length: 4\r\nContent-Type: application/json \r\n\r\n{\"a\"").unwrap();
    assert_eq!(post.body, b"{\"a\"");
    assert_eq!(post.content_type.as_deref(), Some("application/json"));
    assert_eq!(get.content_type, None);
}

#[test]
fn a_request_this_server_does_not_read_is_an_error() {
    for bad in [
        "",
        "GET\r\n\r\n",
        "GET / HTTP/1.1\r\nContent-Length: x\r\n\r\n",
        "POST / HTTP/1.1\r\nContent-Length: 999999\r\n\r\n",
        "GET / HTTP/1.1\r\nHost: cut",
    ] {
        read(bad).unwrap_err();
    }
    let long = format!("GET /{} HTTP/1.1\r\n\r\n", "x".repeat(9000));
    read(&long).unwrap_err();
}

#[test]
fn a_response_says_its_status_type_and_length_and_closes() {
    for (status, reason) in [
        (200, "OK"),
        (400, "Bad Request"),
        (404, "Not Found"),
        (415, "Unsupported Media Type"),
        (500, "Internal Server Error"),
    ] {
        let mut out = Vec::new();
        Response {
            status,
            content_type: "text/plain",
            body: b"hi".to_vec(),
        }
        .write(&mut out)
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.starts_with(&format!("HTTP/1.1 {status} {reason}\r\n")),
            "{text}"
        );
        assert!(text.contains("Content-Length: 2\r\n") && text.ends_with("\r\n\r\nhi"));
    }
}

#[test]
fn the_server_answers_each_connection_and_a_garbled_one_with_400() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        serve(&listener, |req| {
            // A reply too big to be sent to a client that has gone.
            let body = if req.path == "/big" {
                vec![b'x'; 16 << 20]
            } else {
                req.path.clone().into_bytes()
            };
            Response::ok("text/plain", body)
        });
    });
    let ask = |text: &str| {
        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(text.as_bytes()).unwrap();
        s.shutdown(std::net::Shutdown::Write).unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        out
    };
    let ok = ask("GET /there HTTP/1.1\r\n\r\n");
    assert!(
        ok.starts_with("HTTP/1.1 200 OK") && ok.ends_with("/there"),
        "{ok}"
    );
    let bad = ask("nonsense");
    assert!(bad.starts_with("HTTP/1.1 400"), "{bad}");
    let mut gone = TcpStream::connect(addr).unwrap();
    gone.write_all(b"GET /big HTTP/1.1\r\n\r\n").unwrap();
    drop(gone);
    // The server goes on to the next connection.
    let after = ask("GET /after HTTP/1.1\r\n\r\n");
    assert!(after.ends_with("/after"), "{after}");
}
