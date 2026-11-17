use serde_json::Value;
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::Duration,
};
use tempfile::TempDir;
#[cfg(unix)]
mod common;

struct Dashboard {
    child: Child,
    port: u16,
    _project: TempDir,
}
impl Drop for Dashboard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Dashboard {
    fn start() -> Self {
        Self::start_with(|_, _| {})
    }

    fn start_with(configure: impl FnOnce(&std::path::Path, &mut Command)) -> Self {
        let project = TempDir::new().unwrap();
        fs::write(
            project.path().join("hello & world.rs"),
            "human();\n<script>alert('source')</script>\n",
        )
        .unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_aimark"));
        command
            .current_dir(project.path())
            .arg("--root")
            .arg(project.path())
            .args(["dashboard", "--port", "0"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        configure(project.path(), &mut command);
        let mut child = command.spawn().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (send, receive) = mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            let mut reader = BufReader::new(stdout);
            let result = reader.read_line(&mut line);
            let _ = send.send((result, line));
            // Keep draining output; closing the pipe here races the server's next println.
            let _ = std::io::copy(&mut reader, &mut std::io::sink());
        });
        let (_, line) = receive
            .recv_timeout(Duration::from_secs(15))
            .expect("dashboard did not start");
        let port = line.trim().rsplit(':').next().unwrap().parse().unwrap();
        Self {
            child,
            port,
            _project: project,
        }
    }
    fn request(&self, method: &str, url: &str, extra: &str) -> String {
        Self::request_port(self.port, method, url, extra, Duration::from_secs(10))
    }

    fn request_port(port: u16, method: &str, url: &str, extra: &str, timeout: Duration) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream.set_read_timeout(Some(timeout)).unwrap();
        write!(
            stream,
            "{method} {url} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n{extra}\r\n",
            port
        )
        .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }
}

#[test]
fn dashboard_serves_embedded_assets_and_read_only_provenance() {
    let d = Dashboard::start();
    for url in ["/", "/app.js", "/style.css"] {
        let response = d.request("GET", url, "");
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.to_lowercase().contains("content-security-policy:"));
    }
    let response = d.request("GET", "/api/stat", "");
    let (_, body) = response.split_once("\r\n\r\n").unwrap();
    let stats: Value = serde_json::from_str(body).unwrap();
    assert_eq!(stats["total"], 2);
    assert_eq!(stats["pending"], 2);
    let response = d.request("GET", "/api/blame?file=hello+%26+world.rs", "");
    let (_, body) = response.split_once("\r\n\r\n").unwrap();
    let lines: Value = serde_json::from_str(body).unwrap();
    assert_eq!(lines[1]["text"], "<script>alert('source')</script>");
    let output = Command::new("git")
        .current_dir(d._project.path())
        .args(["--git-dir=.aimark/repo", "log", "--format=%s"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8(output.stdout).unwrap().lines().count(), 1);
}

#[test]
fn dashboard_rejects_mutation_cross_origin_and_path_escapes() {
    let d = Dashboard::start();
    assert!(
        d.request("POST", "/api/stat", "")
            .starts_with("HTTP/1.1 405")
    );
    assert!(
        d.request("GET", "/api/stat", "Origin: https://example.invalid\r\n")
            .starts_with("HTTP/1.1 403")
    );
    for url in [
        "/api/blame?file=../secret",
        "/api/blame?file=.aimark/repo/config",
        "/api/blame?file=one&file=two",
    ] {
        assert!(d.request("GET", url, "").starts_with("HTTP/1.1 400"));
    }
    assert!(d.request("GET", "/missing", "").starts_with("HTTP/1.1 404"));
}

#[cfg(unix)]
#[test]
fn blocked_report_does_not_block_assets_or_edit_hooks() {
    let gate = common::GitGate::new();
    let dashboard = Dashboard::start_with(|root, command| {
        let output = Command::new(env!("CARGO_BIN_EXE_aimark"))
            .arg("--root")
            .arg(root)
            .arg("checkpoint")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        gate.configure(command);
    });
    let port = dashboard.port;
    let report = std::thread::spawn(move || {
        Dashboard::request_port(port, "GET", "/api/stat", "", Duration::from_secs(15))
    });
    gate.wait_until_blocked();
    let asset = Dashboard::request_port(port, "GET", "/style.css", "", Duration::from_secs(2));
    assert!(asset.starts_with("HTTP/1.1 200"));

    let file = dashboard._project.path().join("hello & world.rs");
    let call_hook = |event: &str| {
        let mut child = Command::new(env!("CARGO_BIN_EXE_aimark"))
            .arg("--root")
            .arg(dashboard._project.path())
            .args(["hook", "claude"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let payload = serde_json::json!({"hook_event_name":event,"tool_name":"Write","tool_input":{"file_path":file}});
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.to_string().as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{event}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    call_hook("PreToolUse");
    fs::write(&file, "robot();\n").unwrap();
    call_hook("PostToolUse");
    gate.unblock();
    let response = report.join().unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    let stats: Value = serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(stats["total"], 2);
    assert_eq!(stats["ai"], 0);
    let response = dashboard.request("GET", "/api/stat", "");
    let stats: Value = serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(stats["ai"], 1);
}
