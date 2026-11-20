//! The history dashboard: a small HTTP server on 127.0.0.1 with one page and a JSON API.
//!
//! - `GET /` the page; `GET /api/blocks?from=&to=` blocks overlapping a window (Unix seconds);
//!   `GET /api/projects` all-time totals and project folders; `GET /api/live` what is counting;
//!   `GET /api/export.csv?from=&to=` the window as CSV.
//! - `PATCH /api/blocks/<id>` with `{project, start, end}` and `DELETE /api/blocks/<id>`.
//! - `POST /api/sync` syncs with the other machines now (the page does it when it opens).
//!
//! Only this machine can reach it. Requests must name 127.0.0.1 or localhost in `Host`, which
//! stops DNS-rebinding pages, and changes need an `X-Codeclock` header, which a page from another
//! origin cannot send without a CORS preflight that this server never approves.

use std::io::Read;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use chrono::{Local, TimeZone};
use serde::Deserialize;
use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response, Server};

use crate::project::Projects;
use crate::store::{Block, Store};
use crate::sync;

const PAGE: &str = include_str!("web/index.html");
const MAX_BODY: u64 = 16 * 1024;

/// What the daemon is counting right now, shared with the server.
#[derive(Debug, Default, Clone)]
pub struct Live {
    pub project: Option<String>,
    pub block: Option<i64>,
}

pub type SharedLive = Arc<Mutex<Live>>;

pub fn url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/")
}

/// Starts the server on a background thread.
pub fn spawn(port: u16, db: PathBuf, roots: Vec<PathBuf>, live: SharedLive, sync: Option<sync::Worker>) -> Result<()> {
    let server = Server::http(("127.0.0.1", port)).map_err(|e| anyhow!("port {port}: {e}"))?;
    let store = Store::open(&db)?;
    std::thread::Builder::new().name("dashboard".into()).spawn(move || {
        let app = App { port, store, roots, live, sync };
        for req in server.incoming_requests() {
            app.handle(req);
        }
    })?;
    log::info!("dashboard at {}", url(port));
    Ok(())
}

/// Runs the server in the foreground (for `codeclock serve`).
pub fn serve(port: u16, db: PathBuf, roots: Vec<PathBuf>, sync: Option<sync::Worker>) -> Result<()> {
    let server = Server::http(("127.0.0.1", port)).map_err(|e| anyhow!("port {port}: {e}"))?;
    let app = App { port, store: Store::open(&db)?, roots, live: SharedLive::default(), sync };
    println!("Dashboard at {}", url(port));
    for req in server.incoming_requests() {
        app.handle(req);
    }
    Ok(())
}

struct App {
    port: u16,
    store: Store,
    roots: Vec<PathBuf>,
    live: SharedLive,
    sync: Option<sync::Worker>,
}

/// A finished response: status, content type, body and extra headers.
struct Reply {
    status: u16,
    kind: &'static str,
    body: Vec<u8>,
    extra: Vec<(&'static str, String)>,
}

impl Reply {
    fn json(status: u16, v: Value) -> Self {
        Reply { status, kind: "application/json", body: v.to_string().into_bytes(), extra: vec![] }
    }
    fn error(status: u16, msg: &str) -> Self {
        Self::json(status, json!({ "error": msg }))
    }
}

#[derive(Deserialize)]
struct BlockEdit {
    project: String,
    start: i64,
    end: i64,
}

impl App {
    fn handle(&self, mut req: Request) {
        let reply = self.route(&mut req).unwrap_or_else(|e| {
            log::error!("dashboard: {e:#}");
            Reply::error(500, "internal error")
        });
        let mut resp = Response::from_data(reply.body).with_status_code(reply.status);
        let mut headers = vec![
            ("Content-Type", format!("{}; charset=utf-8", reply.kind)),
            ("Cache-Control", "no-store".into()),
            ("X-Content-Type-Options", "nosniff".into()),
            ("Referrer-Policy", "no-referrer".into()),
        ];
        headers.extend(reply.extra);
        for (k, v) in headers {
            if let Ok(h) = Header::from_bytes(k.as_bytes(), v.as_bytes()) {
                resp.add_header(h);
            }
        }
        let _ = req.respond(resp);
    }

    fn header<'a>(req: &'a Request, name: &str) -> Option<&'a str> {
        req.headers().iter().find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name)).map(|h| h.value.as_str())
    }

    fn allowed_host(&self, value: &str) -> bool {
        value == format!("127.0.0.1:{}", self.port) || value == format!("localhost:{}", self.port)
    }

    fn route(&self, req: &mut Request) -> Result<Reply> {
        if !Self::header(req, "Host").is_some_and(|h| self.allowed_host(h)) {
            return Ok(Reply::error(403, "unexpected host"));
        }
        let url = req.url().to_string();
        let (path, query) = url.split_once('?').unwrap_or((url.as_str(), ""));
        let method = req.method().clone();

        if matches!(method, Method::Post | Method::Patch | Method::Delete) {
            let origin_ok = Self::header(req, "Origin").is_none_or(|o| {
                o.strip_prefix("http://").is_some_and(|h| self.allowed_host(h))
            });
            if Self::header(req, "X-Codeclock").is_none() || !origin_ok {
                return Ok(Reply::error(403, "changes must come from the dashboard page"));
            }
        }

        match (&method, path) {
            (Method::Get, "/") => Ok(Reply {
                status: 200,
                kind: "text/html",
                body: PAGE.as_bytes().to_vec(),
                extra: vec![(
                    "Content-Security-Policy",
                    "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; \
                     connect-src 'self'; img-src data:; frame-ancestors 'none'; form-action 'none'"
                        .into(),
                )],
            }),
            (Method::Get, "/api/live") => Ok(Reply::json(200, self.live_json())),
            (Method::Get, "/api/projects") => {
                let projects: Vec<Value> = self
                    .store
                    .projects()?
                    .into_iter()
                    .map(|(name, total)| json!({ "name": name, "total": total }))
                    .collect();
                let folders = Projects::scan(&self.roots).names();
                Ok(Reply::json(
                    200,
                    json!({
                        "projects": projects,
                        "folders": folders,
                        "machines": self.store.machines()?,
                        "first": self.store.first_start()?,
                    }),
                ))
            }
            (Method::Get, "/api/blocks") => {
                let Some((from, to)) = window(query) else {
                    return Ok(Reply::error(400, "from and to must be numbers with from < to"));
                };
                Ok(Reply::json(
                    200,
                    json!({ "blocks": self.store.blocks(from, to)?, "now": Local::now().timestamp(), "live": self.live_json() }),
                ))
            }
            (Method::Get, "/api/export.csv") => {
                let Some((from, to)) = window(query) else {
                    return Ok(Reply::error(400, "from and to must be numbers with from < to"));
                };
                let name = format!("codeclock-{}-{}.csv", day(from), day(to - 1));
                Ok(Reply {
                    status: 200,
                    kind: "text/csv",
                    body: to_csv(&self.store.blocks(from, to)?, from, to).into_bytes(),
                    extra: vec![("Content-Disposition", format!("attachment; filename=\"{name}\""))],
                })
            }
            (Method::Post, "/api/sync") => Ok(match &self.sync {
                None => Reply::error(400, "sync is not set up on this machine"),
                Some(w) => match w.sync_now(std::time::Duration::from_secs(30)) {
                    Ok(o) => Reply::json(200, json!({ "outcome": o, "sync": self.sync_status() })),
                    Err(e) => Reply::json(200, json!({ "error": e, "sync": self.sync_status() })),
                },
            }),
            (m @ (Method::Patch | Method::Delete), p) if p.starts_with("/api/blocks/") => {
                let Ok(id) = p["/api/blocks/".len()..].parse::<i64>() else {
                    return Ok(Reply::error(404, "no such block"));
                };
                if self.live.lock().map(|l| l.block == Some(id)).unwrap_or(false) {
                    return Ok(Reply::error(409, "this block is still being counted; edit it once it ends"));
                }
                if *m == Method::Delete {
                    return Ok(if self.store.delete_block(id)? {
                        Reply::json(200, json!({ "ok": true }))
                    } else {
                        Reply::error(404, "no such block")
                    });
                }
                let mut body = String::new();
                req.as_reader().take(MAX_BODY).read_to_string(&mut body)?;
                let Ok(edit) = serde_json::from_str::<BlockEdit>(&body) else {
                    return Ok(Reply::error(400, "expected {project, start, end}"));
                };
                self.edit(id, edit)
            }
            _ => Ok(Reply::error(404, "not found")),
        }
    }

    fn edit(&self, id: i64, e: BlockEdit) -> Result<Reply> {
        let project = e.project.trim();
        if project.is_empty() || project.len() > 200 || project.contains(['/', '\n']) {
            return Ok(Reply::error(400, "the project name must be 1 to 200 characters, without / or line breaks"));
        }
        if e.start >= e.end {
            return Ok(Reply::error(400, "the end must be after the start"));
        }
        if e.end > Local::now().timestamp() + 60 {
            return Ok(Reply::error(400, "a block cannot end in the future"));
        }
        if self.store.overlaps(id, e.start, e.end)? {
            return Ok(Reply::error(409, "these times overlap another block of this project"));
        }
        Ok(if self.store.update_block(id, project, e.start, e.end)? {
            Reply::json(200, json!({ "ok": true }))
        } else {
            Reply::error(404, "no such block")
        })
    }

    fn live_json(&self) -> Value {
        let live = self.live.lock().map(|l| l.clone()).unwrap_or_default();
        json!({ "project": live.project, "block": live.block, "sync": self.sync_status() })
    }

    fn sync_status(&self) -> Value {
        sync::status(&self.store, &sync::repo_dir())
    }
}

/// `from` and `to` from a query string such as `from=1&to=2`.
fn window(query: &str) -> Option<(i64, i64)> {
    let get = |key: &str| {
        query
            .split('&')
            .filter_map(|kv| kv.split_once('='))
            .find(|(k, _)| *k == key)
            .and_then(|(_, v)| v.parse::<i64>().ok())
    };
    let (from, to) = (get("from")?, get("to")?);
    (from < to).then_some((from, to))
}

fn local(ts: i64) -> String {
    Local.timestamp_opt(ts, 0).single().map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string()).unwrap_or_default()
}

fn day(ts: i64) -> String {
    local(ts).get(..10).unwrap_or("").to_string()
}

/// Blocks clipped to the window, so the durations add up to the dashboard's totals.
fn to_csv(blocks: &[Block], from: i64, to: i64) -> String {
    let mut out = String::from("id,project,machine,editor,start,end,duration_secs\n");
    for b in blocks {
        let (s, e) = (b.start.max(from), b.end.min(to));
        out.push_str(&format!(
            "{},{},{},{},{},{},{}\n",
            b.id,
            csv_field(&b.project),
            csv_field(&b.machine),
            csv_field(&b.editor),
            local(s),
            local(e),
            e - s
        ));
    }
    out
}

/// Quotes when needed, and stops spreadsheets from reading a value as a formula.
fn csv_field(v: &str) -> String {
    let v = if v.starts_with(['=', '+', '-', '@']) { format!("'{v}") } else { v.to_string() };
    if v.contains([',', '"', '\n', '\r']) { format!("\"{}\"", v.replace('"', "\"\"")) } else { v }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_window() {
        assert_eq!(window("from=10&to=20"), Some((10, 20)));
        assert_eq!(window("to=20&x=1&from=10"), Some((10, 20)));
        assert_eq!(window("from=20&to=10"), None);
        assert_eq!(window("from=a&to=10"), None);
        assert_eq!(window(""), None);
    }

    #[test]
    fn csv_is_clipped_and_escaped() {
        let b = Block {
            id: 1,
            uuid: "u".into(),
            machine: "laptop-linux".into(),
            project: "a,b".into(),
            editor: "=cmd".into(),
            start: 100,
            end: 400,
        };
        let csv = to_csv(&[b], 200, 300);
        let line = csv.lines().nth(1).unwrap();
        assert!(line.starts_with("1,\"a,b\",laptop-linux,'=cmd,"));
        assert!(line.ends_with(",100"));
    }
}
