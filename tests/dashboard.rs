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
        let project = TempDir::new().unwrap();
        fs::write(
            project.path().join("hello & world.rs"),
            "human();\n<script>alert('source')</script>\n",
        )
        .unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_aimark"))
            .current_dir(project.path())
            .arg("--root")
            .arg(project.path())
            .args(["dashboard", "--port", "0"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
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
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        write!(
            stream,
            "{method} {url} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n{extra}\r\n",
            self.port
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
