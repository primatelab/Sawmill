//! Drives the built binary over a real pipe, which the unit tests in
//! `src/mcp.rs` cannot: argument handling, the stdio loop and the framing.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

const LOG: &str = "\
2023-09-18 15:54:49 INFO  starting run 1
2023-09-18 15:55:02 ERROR cannot reach 10.0.0.2
2023-09-18 15:56:30 INFO  finished
";

fn temp_log(label: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("sawmill-pipe-{}-{label}.log", std::process::id()));
    std::fs::write(&path, LOG).unwrap();
    path
}

/// Send each request down stdin, then read one reply per line.
fn talk(args: &[&str], requests: &[&str]) -> Vec<serde_json::Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_sawmill"))
        .arg("--mcp")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("sawmill starts");
    {
        let mut stdin = child.stdin.take().unwrap();
        for request in requests {
            writeln!(stdin, "{request}").unwrap();
        }
    }
    let out = BufReader::new(child.stdout.take().unwrap());
    let replies: Vec<serde_json::Value> = out
        .lines()
        .map(|l| serde_json::from_str(&l.unwrap()).expect("every line is one JSON message"))
        .collect();
    assert!(child.wait().unwrap().success(), "closing stdin ends the server cleanly");
    replies
}

fn text(reply: &serde_json::Value) -> String {
    assert!(reply.get("error").is_none(), "{reply}");
    assert_eq!(reply["result"]["isError"], false, "{reply}");
    reply["result"]["content"][0]["text"].as_str().unwrap().to_string()
}

#[test]
fn a_client_can_handshake_list_tools_and_read_a_log() {
    let path = temp_log("session");
    let open = format!(
        r#"{{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{{"name":"open_logs","arguments":{{"paths":["{}"]}}}}}}"#,
        path.display()
    );
    let replies = talk(
        &[],
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            &open,
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"lines","arguments":{"limit":1}}}"#,
            r#"{"jsonrpc":"2.0","id":5,"method":"ping"}"#,
        ],
    );
    let _ = std::fs::remove_file(&path);

    assert_eq!(replies.len(), 5, "the notification is answered with silence");
    assert_eq!(replies.iter().map(|r| r["id"].as_i64().unwrap()).collect::<Vec<_>>(), [1, 2, 3, 4, 5]);
    assert_eq!(replies[0]["result"]["protocolVersion"], "2025-06-18");
    let names: Vec<&str> = replies[1]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        ["open_logs", "close_logs", "overview", "set_filters", "patterns", "pattern_lines", "lines", "context", "timeline"]
    );
    assert!(text(&replies[2]).contains("\"total\":3"), "{}", text(&replies[2]));
    assert!(text(&replies[3]).contains("starting run 1"));
    assert_eq!(replies[4]["result"], serde_json::json!({}));
}

#[test]
fn files_named_on_the_command_line_arrive_in_the_default_session() {
    let path = temp_log("preload");
    let replies = talk(
        &[path.to_str().unwrap()],
        &[r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"overview","arguments":{}}}"#],
    );
    let _ = std::fs::remove_file(&path);
    let out: serde_json::Value = serde_json::from_str(&text(&replies[0])).unwrap();
    assert_eq!(out["lines"], serde_json::json!({ "total": 3, "matching": 3 }));
    assert_eq!(out["span"]["to"], "2023-09-18 15:56:30");
}

#[test]
fn stdin_is_the_transport_and_is_never_read_as_a_log() {
    let replies = talk(
        &[],
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"overview","arguments":{}}}"#,
            "not json at all",
            r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#,
        ],
    );
    assert_eq!(replies[0]["result"]["isError"], true, "no log is loaded from a piped stdin");
    assert!(replies[0]["result"]["content"][0]["text"].as_str().unwrap().contains("open_logs"));
    assert_eq!(replies[1]["error"]["code"], -32700);
    assert_eq!(replies[2]["id"], 2, "a bad frame does not stop the loop");
}
