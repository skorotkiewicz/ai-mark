use crate::{ledger::Ledger, report};
use anyhow::{Context, Result, ensure};
use serde_json::json;
use std::{
    io::Write,
    net::{Ipv4Addr, SocketAddrV4},
    path::{Path, PathBuf},
};
use tiny_http::{Header, Method, Response, Server};

pub fn serve(root: PathBuf, port: u16) -> Result<()> {
    let server = Server::http(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))
        .map_err(|e| anyhow::anyhow!("could not start dashboard: {e}"))?;
    let addr = server
        .server_addr()
        .to_ip()
        .context("dashboard has no TCP address")?;
    println!("aimark dashboard: http://{addr}");
    println!("Press Ctrl-C to stop. Files stay on this machine.");
    std::io::stdout().flush()?;
    for request in server.incoming_requests() {
        let host = request
            .headers()
            .iter()
            .find(|h| h.field.equiv("Host"))
            .map(|h| h.value.as_str())
            .unwrap_or("");
        let origin = request
            .headers()
            .iter()
            .find(|h| h.field.equiv("Origin"))
            .map(|h| h.value.as_str());
        let allowed = [addr.to_string(), format!("localhost:{}", addr.port())];
        let trusted = allowed.iter().any(|h| h == host)
            && origin.is_none_or(|o| allowed.iter().any(|h| o == format!("http://{h}")));
        let (status, kind, body) = if !trusted {
            (
                403,
                "application/json",
                json!({"error":"untrusted host or origin"}).to_string(),
            )
        } else if request.method() != &Method::Get {
            (
                405,
                "application/json",
                json!({"error":"only GET is supported"}).to_string(),
            )
        } else {
            match route(&root, request.url()) {
                Ok(value) => value,
                Err(error) => (
                    400,
                    "application/json",
                    json!({"error":error.to_string()}).to_string(),
                ),
            }
        };
        let mut response = Response::from_string(body).with_status_code(status);
        for (name,value) in [
            ("Content-Type",format!("{kind}; charset=utf-8")), ("Cache-Control","no-store".into()),
            ("X-Content-Type-Options","nosniff".into()), ("Referrer-Policy","no-referrer".into()), ("X-Frame-Options","DENY".into()),
            ("Content-Security-Policy","default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'".into()),
        ] { response.add_header(Header::from_bytes(name,value).expect("constant HTTP header")); }
        if let Err(error) = request.respond(response) {
            eprintln!("dashboard response failed: {error}");
        }
    }
    Ok(())
}

fn route(root: &Path, url: &str) -> Result<(u16, &'static str, String)> {
    let (path, query) = url.split_once('?').unwrap_or((url, ""));
    let body = match path {
        "/" => return Ok((200, "text/html", include_str!("../web/index.html").into())),
        "/app.js" => return Ok((200, "text/javascript", include_str!("../web/app.js").into())),
        "/style.css" => return Ok((200, "text/css", include_str!("../web/style.css").into())),
        "/api/stat" => serde_json::to_string(&report::stats(&Ledger::open(root.to_owned())?)?)?,
        "/api/blame" => {
            let params: Vec<_> = form_urlencoded::parse(query.as_bytes()).collect();
            ensure!(
                params.len() == 1 && params[0].0 == "file",
                "provide exactly one file parameter"
            );
            let ledger = Ledger::open(root.to_owned())?;
            let file = ledger.path(Path::new(params[0].1.as_ref()), root)?;
            serde_json::to_string(&report::blame(&ledger, &file)?)?
        }
        _ => {
            return Ok((
                404,
                "application/json",
                json!({"error":"not found"}).to_string(),
            ));
        }
    };
    Ok((200, "application/json", body))
}
