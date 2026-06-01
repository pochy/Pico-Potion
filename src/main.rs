use axum::{
    extract::{Form, Path, State},
    http::StatusCode,
    response::{Html, IntoResponse, Redirect},
    routing::{get, post},
    Json, Router,
};
use rusqlite::Connection;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

const DB_FILE: &str = "pico_potion.db";
const DEFAULT_PORT: u16 = 8080;
const ENV_PORT: &str = "PICO_POTION_PORT";

const MARKED_JS: &str = include_str!("../assets/marked.umd.min.js");

type DbState = Arc<Mutex<Connection>>;

struct PageMeta {
    id: String,
    title: String,
}

#[derive(serde::Deserialize)]
struct SaveForm {
    id: String,
    title: String,
    content: String,
}

#[derive(serde::Serialize)]
struct MemoryStats {
    rss_kb: u64,
    rss_mb: f64,
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    migrate_legacy_db();
    let conn = Connection::open(DB_FILE).expect("Failed to open DB");
    init_db(&conn);

    let db_state = Arc::new(Mutex::new(conn));

    let app = Router::new()
        .route("/", get(handle_root))
        .route("/page/:id", get(handle_page))
        .route("/save", post(handle_save))
        .route("/pages/new", post(handle_new_page))
        .route("/pages/:id/delete", post(handle_delete_page))
        .route("/stats/memory", get(handle_memory))
        .with_state(db_state);

    let port = listen_port();
    let addr = format!("0.0.0.0:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap_or_else(|e| {
        eprintln!("Failed to bind {addr}: {e}");
        std::process::exit(1);
    });
    println!("🚀 Pico Potion started on http://localhost:{port}");
    axum::serve(listener, app).await.unwrap();
}

fn listen_port() -> u16 {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print_usage();
                std::process::exit(0);
            }
            "-p" | "--port" => {
                let Some(value) = args.next() else {
                    eprintln!("error: --port requires a value");
                    std::process::exit(1);
                };
                return parse_port(&value);
            }
            s if !s.starts_with('-') => return parse_port(s),
            other => {
                eprintln!("error: unknown argument '{other}'");
                print_usage();
                std::process::exit(1);
            }
        }
    }
    match std::env::var(ENV_PORT) {
        Ok(value) => parse_port(&value),
        Err(_) => DEFAULT_PORT,
    }
}

fn parse_port(s: &str) -> u16 {
    match s.parse::<u16>() {
        Ok(0) => {
            eprintln!("error: port must be 1–65535");
            std::process::exit(1);
        }
        Ok(port) => port,
        Err(_) => {
            eprintln!("error: invalid port '{s}'");
            std::process::exit(1);
        }
    }
}

async fn handle_memory() -> Result<Json<MemoryStats>, StatusCode> {
    let rss_kb = process_rss_kb().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    Ok(Json(MemoryStats {
        rss_kb,
        rss_mb: rss_kb as f64 / 1024.0,
    }))
}

fn process_rss_kb() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        linux_rss_kb()
    }
    #[cfg(target_os = "macos")]
    {
        macos_rss_kb()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = ();
        None
    }
}

#[cfg(target_os = "linux")]
fn linux_rss_kb() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    parse_vm_rss_kib(&status)
}

#[cfg(target_os = "macos")]
fn macos_rss_kb() -> Option<u64> {
    use std::ffi::c_void;
    use std::mem;

    #[repr(C)]
    struct ProcTaskinfo {
        virtual_size: u64,
        resident_size: u64,
        total_user: u64,
        total_system: u64,
        threads_user: u64,
        threads_system: u64,
        policy: i32,
        faults: i32,
        pageins: i32,
        cow_faults: i32,
        messages_sent: i32,
        messages_received: i32,
        syscalls_mach: i32,
        syscalls_unix: i32,
        csw: i32,
        threadnum: i32,
        numrunning: i32,
        priority: i32,
    }

    const PROC_PIDTASKINFO: i32 = 4;

    #[link(name = "proc")]
    extern "C" {
        fn proc_pidinfo(
            pid: i32,
            flavor: i32,
            arg: u64,
            buffer: *mut c_void,
            buffersize: i32,
        ) -> i32;
    }

    let mut info: ProcTaskinfo = unsafe { mem::zeroed() };
    let size = mem::size_of::<ProcTaskinfo>() as i32;
    let pid = i32::try_from(std::process::id()).ok()?;
    let written = unsafe {
        proc_pidinfo(
            pid,
            PROC_PIDTASKINFO,
            0,
            &mut info as *mut _ as *mut c_void,
            size,
        )
    };
    if written != size {
        return None;
    }
    Some(info.resident_size / 1024)
}

#[cfg(any(target_os = "linux", test))]
fn parse_vm_rss_kib(status: &str) -> Option<u64> {
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            return rest.split_whitespace().next()?.parse().ok();
        }
    }
    None
}

fn print_usage() {
    eprintln!(
        "Pico Potion — ultra-light shared notes\n\
         \n\
         Usage: pico_potion [OPTIONS] [PORT]\n\
         \n\
         Options:\n\
           -p, --port <PORT>   Listen port (default: {DEFAULT_PORT})\n\
           -h, --help          Show this help\n\
         \n\
         Environment:\n\
           {ENV_PORT}          Same as --port (CLI takes precedence)"
    );
}

fn migrate_legacy_db() {
    if std::path::Path::new(DB_FILE).exists() {
        return;
    }
    let legacy = "micro_notion.db";
    if std::path::Path::new(legacy).exists() {
        let _ = std::fs::rename(legacy, DB_FILE);
    }
}

fn init_db(conn: &Connection) {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS pages (id TEXT PRIMARY KEY, content TEXT)",
        [],
    )
    .expect("Failed to create table");

    let _ = conn.execute(
        "ALTER TABLE pages ADD COLUMN title TEXT NOT NULL DEFAULT '無題'",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE pages ADD COLUMN created_at INTEGER NOT NULL DEFAULT 0",
        [],
    );

    let now = now_secs();
    let _ = conn.execute(
        "UPDATE pages SET title = '家族の共有ノート', created_at = ?1 WHERE id = 'home' AND created_at = 0",
        [now],
    );

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM pages", [], |row| row.get(0))
        .unwrap_or(0);
    if count == 0 {
        conn.execute(
            "INSERT INTO pages (id, title, content, created_at) VALUES ('home', '家族の共有ノート', '', ?1)",
            [now],
        )
        .expect("Failed to seed default page");
    }
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn gen_page_id() -> String {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();
    format!("p{ms}")
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn is_legacy_html(s: &str) -> bool {
    let t = s.trim_start();
    t.starts_with('<') || t.contains("<p>") || t.contains("<h1")
}

fn list_pages(conn: &Connection) -> Vec<PageMeta> {
    let mut stmt = conn
        .prepare("SELECT id, title FROM pages ORDER BY created_at ASC")
        .unwrap();
    stmt.query_map([], |row| {
        Ok(PageMeta {
            id: row.get(0)?,
            title: row.get(1)?,
        })
    })
    .unwrap()
    .filter_map(|r| r.ok())
    .collect()
}

fn render_sidebar(current_id: &str, pages: &[PageMeta]) -> String {
    pages
        .iter()
        .map(|p| {
            let active = if p.id == current_id { " active" } else { "" };
            format!(
                r#"<a href="/page/{}" class="page-item{}"><span class="page-icon" aria-hidden="true"></span><span class="page-title">{}</span></a>"#,
                escape_html(&p.id),
                active,
                escape_html(&p.title)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn escape_js_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '<' => out.push_str("\\u003C"),
            '>' => out.push_str("\\u003E"),
            '&' => out.push_str("\\u0026"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn render_page_index_json(pages: &[PageMeta]) -> String {
    let items = pages
        .iter()
        .map(|p| {
            format!(
                r#"{{"id":"{}","title":"{}"}}"#,
                escape_js_string(&p.id),
                escape_js_string(&p.title)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("[{}]", items)
}

async fn handle_root(State(db): State<DbState>) -> Redirect {
    let conn = db.lock().unwrap();
    let id: String = conn
        .query_row(
            "SELECT id FROM pages ORDER BY created_at ASC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap_or_else(|_| "home".to_string());
    Redirect::to(&format!("/page/{}", id))
}

async fn handle_page(State(db): State<DbState>, Path(id): Path<String>) -> impl IntoResponse {
    let conn = db.lock().unwrap();
    let page = conn.query_row(
        "SELECT title, content FROM pages WHERE id = ?1",
        [&id],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
    );

    match page {
        Ok((title, content)) => {
            let pages = list_pages(&conn);
            Html(get_html_template(&id, &title, &content, &pages)).into_response()
        }
        Err(_) => Redirect::to("/").into_response(),
    }
}

async fn handle_save(State(db): State<DbState>, Form(form): Form<SaveForm>) {
    let conn = db.lock().unwrap();
    let title = if form.title.trim().is_empty() {
        "無題".to_string()
    } else {
        form.title
    };
    conn.execute(
        "UPDATE pages SET title = ?1, content = ?2 WHERE id = ?3",
        [&title, &form.content, &form.id],
    )
    .unwrap();
}

async fn handle_new_page(State(db): State<DbState>) -> Redirect {
    let id = gen_page_id();
    let conn = db.lock().unwrap();
    conn.execute(
        "INSERT INTO pages (id, title, content, created_at) VALUES (?1, '無題', '', ?2)",
        [&id, &now_secs().to_string()],
    )
    .unwrap();
    Redirect::to(&format!("/page/{}", id))
}

async fn handle_delete_page(State(db): State<DbState>, Path(id): Path<String>) -> Redirect {
    let conn = db.lock().unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM pages", [], |row| row.get(0))
        .unwrap();

    if count <= 1 {
        conn.execute(
            "UPDATE pages SET title = '無題', content = '' WHERE id = ?1",
            [&id],
        )
        .unwrap();
        return Redirect::to(&format!("/page/{}", id));
    }

    conn.execute("DELETE FROM pages WHERE id = ?1", [&id])
        .unwrap();
    let next_id: String = conn
        .query_row(
            "SELECT id FROM pages ORDER BY created_at ASC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    Redirect::to(&format!("/page/{}", next_id))
}

fn get_html_template(page_id: &str, title: &str, content: &str, pages: &[PageMeta]) -> String {
    let sidebar = render_sidebar(page_id, pages);
    let page_index = render_page_index_json(pages);
    let legacy_hint = if is_legacy_html(content) {
        r#"<p class="hint hint-legacy">このページは旧 HTML 形式です。Markdown に書き直すとプレビューが正しく表示されます。</p>"#
    } else {
        ""
    };
    format!(
        r#"<!DOCTYPE html>
<html lang="ja">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>{page_title} - Pico Potion</title>
    <style>
        :root {{
            --fg: #37352f;
            --fg-muted: rgba(55, 53, 47, 0.65);
            --fg-faint: rgba(55, 53, 47, 0.45);
            --bg: #ffffff;
            --bg-sidebar: #f7f6f3;
            --bg-hover: rgba(55, 53, 47, 0.08);
            --bg-active: rgba(55, 53, 47, 0.12);
            --border: rgba(55, 53, 47, 0.09);
            --accent: #2383e2;
            --content-w: 708px;
            --font: ui-sans-serif, -apple-system, BlinkMacSystemFont, "Segoe UI", Helvetica, "Apple Color Emoji", Arial, sans-serif;
            --mono: ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace;
        }}
        * {{ box-sizing: border-box; margin: 0; padding: 0; }}
        body {{ font-family: var(--font); color: var(--fg); background: var(--bg); font-size: 16px; line-height: 1.5; -webkit-font-smoothing: antialiased; }}
        .app {{ display: flex; min-height: 100dvh; }}
        .sidebar {{
            width: 260px; min-width: 260px;
            background: var(--bg-sidebar);
            border-right: 1px solid var(--border);
            display: flex; flex-direction: column;
            padding: 8px 10px 12px;
        }}
        .sidebar-header {{
            display: flex; align-items: center; gap: 8px;
            padding: 6px 8px 14px;
            font-size: 14px; font-weight: 600; color: var(--fg);
            letter-spacing: -0.01em;
        }}
        .sidebar-header::before {{
            content: "";
            width: 22px; height: 22px; flex-shrink: 0;
            border-radius: 4px;
            background: #37352f;
        }}
        .new-page-form {{ padding: 0 2px 6px; }}
        .new-page-btn {{
            width: 100%; padding: 5px 8px;
            border: none; background: transparent;
            color: var(--fg-muted); font-size: 14px; font-weight: 500;
            text-align: left; border-radius: 6px; cursor: pointer;
            transition: background 0.12s ease;
        }}
        .new-page-btn:hover {{ background: var(--bg-hover); color: var(--fg); }}
        .page-list {{ flex: 1; overflow-y: auto; padding: 2px 0; }}
        .page-list-label {{
            padding: 8px 8px 4px;
            font-size: 11px; font-weight: 500;
            text-transform: uppercase; letter-spacing: 0.04em;
            color: var(--fg-faint);
        }}
        .page-item {{
            display: flex; align-items: center; gap: 8px;
            padding: 4px 8px; margin: 1px 0;
            border-radius: 6px;
            color: var(--fg); text-decoration: none;
            font-size: 14px; line-height: 1.35;
            transition: background 0.12s ease;
        }}
        .page-item:hover {{ background: var(--bg-hover); }}
        .page-item.active {{ background: var(--bg-active); font-weight: 500; }}
        .page-icon {{
            width: 18px; height: 20px; flex-shrink: 0;
            background: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 16 16' fill='%2337352f' opacity='0.45'%3E%3Cpath d='M4 1h5.5L13 4.5V14a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V2a1 1 0 0 1 1-1zm5 1v3h3'/%3E%3C/svg%3E") center / 16px no-repeat;
        }}
        .page-title {{ overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }}
        .sidebar-footer {{ padding: 10px 2px 0; margin-top: auto; border-top: 1px solid var(--border); }}
        .delete-form {{ width: 100%; }}
        .delete-btn {{
            width: 100%; padding: 5px 8px;
            border: none; background: transparent;
            color: var(--fg-muted); font-size: 13px;
            text-align: left; border-radius: 6px; cursor: pointer;
            transition: background 0.12s ease, color 0.12s ease;
        }}
        .delete-btn:hover {{ background: rgba(235, 87, 87, 0.12); color: #c4564d; }}
        .tool-btn.mem-tool-btn {{
            font-size: 11px; letter-spacing: 0.02em;
            color: var(--accent); font-weight: 600;
        }}
        .main {{
            flex: 1; min-width: 0;
            display: flex; flex-direction: column;
            align-items: center;
            overflow-y: auto;
            background: var(--bg);
        }}
        .page-shell {{
            width: 100%; max-width: calc(var(--content-w) + 192px);
            padding: 0 96px 96px;
        }}
        .page-header {{ padding-top: 72px; padding-bottom: 8px; }}
        #title {{
            width: 100%; border: none; outline: none;
            font-size: 40px; font-weight: 700;
            line-height: 1.2; letter-spacing: -0.03em;
            color: var(--fg); background: transparent;
            text-wrap: balance;
        }}
        #title::placeholder {{ color: rgba(55, 53, 47, 0.35); }}
        .editor-chrome {{
            position: sticky; top: 0; z-index: 10;
            margin: 0 -8px 4px; padding: 8px;
            background: rgba(255, 255, 255, 0.92);
            backdrop-filter: blur(8px);
            border-bottom: 1px solid transparent;
        }}
        .editor-chrome.is-scrolled {{ border-bottom-color: var(--border); }}
        .chrome-row {{
            display: flex; align-items: center; justify-content: space-between;
            gap: 12px; flex-wrap: wrap;
        }}
        .mode-tabs {{
            display: inline-flex; padding: 2px;
            background: rgba(55, 53, 47, 0.06);
            border-radius: 8px;
        }}
        .mode-tab {{
            padding: 5px 12px; border: none;
            background: transparent; color: var(--fg-muted);
            font-size: 13px; font-weight: 500;
            border-radius: 6px; cursor: pointer;
            transition: background 0.12s ease, color 0.12s ease, box-shadow 0.12s ease;
        }}
        .mode-tab:hover {{ color: var(--fg); }}
        .mode-tab.active {{
            background: var(--bg); color: var(--fg);
            box-shadow: 0 1px 2px rgba(15, 15, 15, 0.06);
        }}
        .chrome-actions {{ display: flex; align-items: center; gap: 10px; }}
        .live-toggle {{
            display: inline-flex; align-items: center; gap: 7px;
            color: var(--fg-muted); font-size: 13px;
            user-select: none; cursor: pointer;
        }}
        .live-toggle input {{ accent-color: var(--fg); width: 14px; height: 14px; }}
        .toolbar {{
            display: flex; flex-wrap: wrap; align-items: center;
            gap: 2px; margin-top: 8px;
            padding-top: 8px; border-top: 1px solid var(--border);
        }}
        .tool-btn {{
            min-width: 28px; height: 28px; padding: 0 7px;
            border: none; background: transparent;
            color: var(--fg-muted); border-radius: 6px;
            cursor: pointer; font-size: 13px; font-weight: 500;
            line-height: 1; transition: background 0.12s ease, color 0.12s ease;
        }}
        .tool-btn:hover {{ background: var(--bg-hover); color: var(--fg); }}
        .tool-btn[data-action="bold"] {{ font-weight: 700; }}
        .tool-btn[data-action="italic"] {{ font-style: italic; }}
        .tool-divider {{ width: 1px; height: 18px; margin: 0 4px; background: var(--border); }}
        .hint {{
            color: var(--fg-faint); font-size: 12px;
            margin: 12px 0 0; line-height: 1.5;
            text-wrap: pretty;
        }}
        .hint code {{
            font-family: var(--mono); font-size: 11px;
            padding: 1px 5px; border-radius: 4px;
            background: rgba(135, 131, 120, 0.15);
            color: var(--fg-muted);
        }}
        .hint-legacy {{
            color: #9f6b00; margin-top: 8px;
            padding: 10px 12px; border-radius: 6px;
            background: rgba(255, 171, 0, 0.12);
            border: none;
        }}
        .workbench {{ display: grid; grid-template-columns: 1fr; gap: 24px; align-items: start; }}
        .workbench.live {{ grid-template-columns: minmax(0, 1fr) minmax(0, 1fr); }}
        #editor-wrap, #preview-wrap {{ min-width: 0; }}
        #md-editor {{
            width: 100%; min-height: 60vh;
            padding: 4px 2px 24px;
            font-family: var(--font); font-size: 16px;
            line-height: 1.65; color: var(--fg);
            border: none; resize: none; outline: none;
            background: transparent;
            text-wrap: pretty;
        }}
        #md-editor::placeholder {{ color: var(--fg-faint); }}
        .hidden {{ display: none !important; }}
        .preview-panel {{
            padding: 4px 0 24px;
            border-left: 1px solid var(--border);
            padding-left: 24px;
        }}
        .workbench:not(.live) #preview-wrap .preview-panel {{ border-left: none; padding-left: 0; }}
        .preview-tools {{
            display: flex; flex-wrap: wrap; align-items: center;
            justify-content: space-between; gap: 8px;
            margin-bottom: 16px; color: var(--fg-faint); font-size: 12px;
        }}
        .preview-meta {{ display: flex; flex-wrap: wrap; gap: 6px; }}
        .meta-pill, .tag-pill {{
            display: inline-flex; align-items: center;
            min-height: 22px; padding: 2px 8px;
            border-radius: 4px;
            background: rgba(55, 53, 47, 0.06);
            color: var(--fg-muted); font-size: 12px;
        }}
        .tag-pill {{ color: var(--accent); background: rgba(35, 131, 226, 0.1); }}
        .toc {{
            margin: 0 0 20px; padding: 12px 14px;
            border-radius: 6px;
            background: rgba(55, 53, 47, 0.04);
            border: none;
        }}
        .toc-title {{
            margin-bottom: 8px; color: var(--fg-faint);
            font-size: 11px; font-weight: 600;
            text-transform: uppercase; letter-spacing: 0.04em;
        }}
        .toc a {{
            display: block; padding: 3px 0;
            color: var(--fg-muted); text-decoration: none;
            font-size: 13px; border-radius: 4px;
            transition: color 0.12s ease, background 0.12s ease;
        }}
        .toc a:hover {{ color: var(--accent); }}
        .toc .toc-depth-2 {{ padding-left: 14px; }}
        .toc .toc-depth-3, .toc .toc-depth-4, .toc .toc-depth-5, .toc .toc-depth-6 {{ padding-left: 28px; }}
        #preview {{
            min-height: 40vh; font-size: 16px;
            line-height: 1.7; color: var(--fg);
            overflow-wrap: anywhere; text-wrap: pretty;
        }}
        #preview h1 {{
            font-size: 1.875em; font-weight: 700;
            letter-spacing: -0.02em;
            margin: 1.4em 0 0.25em; line-height: 1.3;
            text-wrap: balance;
        }}
        #preview h1:first-child {{ margin-top: 0; }}
        #preview h2 {{
            font-size: 1.5em; font-weight: 600;
            letter-spacing: -0.015em;
            margin: 1.25em 0 0.2em; line-height: 1.35;
            text-wrap: balance;
        }}
        #preview h3 {{
            font-size: 1.25em; font-weight: 600;
            margin: 1em 0 0.15em; line-height: 1.4;
        }}
        #preview ul, #preview ol {{ padding-left: 1.65em; margin: 0.25em 0 0.5em; }}
        #preview li {{ margin: 0.15em 0; }}
        #preview blockquote {{
            margin: 0.5em 0; padding-left: 14px;
            border-left: 3px solid currentColor;
            color: var(--fg); opacity: 0.85;
        }}
        #preview table {{ width: 100%; border-collapse: collapse; margin: 0.5em 0; font-size: 14px; }}
        #preview th, #preview td {{
            border: 1px solid var(--border);
            padding: 8px 10px; text-align: left; vertical-align: top;
        }}
        #preview th {{ background: rgba(55, 53, 47, 0.04); font-weight: 600; }}
        .table-wrap {{ overflow-x: auto; margin: 0.5em 0; border-radius: 6px; }}
        .code-block {{
            margin: 0.6em 0; border-radius: 6px;
            overflow: hidden; background: #f7f6f3;
            border: 1px solid var(--border);
        }}
        .code-head {{
            display: flex; align-items: center; justify-content: space-between;
            gap: 8px; padding: 6px 10px;
            color: var(--fg-faint); font-size: 12px;
            border-bottom: 1px solid var(--border);
        }}
        .copy-code {{
            border: none; background: transparent;
            color: var(--fg-muted); cursor: pointer; font-size: 12px;
            padding: 2px 6px; border-radius: 4px;
        }}
        .copy-code:hover {{ background: var(--bg-hover); color: var(--fg); }}
        #preview pre {{ padding: 14px 16px; overflow-x: auto; margin: 0; }}
        #preview code {{ font-family: var(--mono); font-size: 0.875em; }}
        #preview :not(pre) > code {{
            background: rgba(135, 131, 120, 0.2);
            padding: 0.15em 0.35em; border-radius: 4px;
            font-size: 0.85em; color: #eb5757;
        }}
        #preview p {{ margin: 0.15em 0 0.5em; min-height: 1.5em; }}
        #preview a, #preview .wiki-link {{ color: inherit; text-decoration: underline; text-underline-offset: 2px; }}
        #preview a:hover {{ color: var(--accent); }}
        #preview mark {{ background: rgba(255, 212, 0, 0.35); padding: 0 0.1em; border-radius: 2px; }}
        #preview del {{ color: var(--fg-faint); }}
        #preview hr {{ border: none; border-top: 1px solid var(--border); margin: 1.5em 0; }}
        .callout {{
            margin: 0.5em 0; padding: 14px 14px 14px 16px;
            border-radius: 6px; background: rgba(55, 53, 47, 0.04);
            display: flex; flex-direction: column; gap: 4px;
        }}
        .callout-title {{
            font-weight: 600; font-size: 14px;
            display: flex; align-items: center; gap: 6px;
        }}
        .callout.note {{ background: rgba(35, 131, 226, 0.08); }}
        .callout.note .callout-title::before {{ content: "ℹ️"; }}
        .callout.tip {{ background: rgba(15, 143, 95, 0.08); }}
        .callout.tip .callout-title::before {{ content: "💡"; }}
        .callout.warning {{ background: rgba(217, 115, 13, 0.1); }}
        .callout.warning .callout-title::before {{ content: "⚠️"; }}
        .callout.important {{ background: rgba(192, 57, 43, 0.08); }}
        .callout.important .callout-title::before {{ content: "❗"; }}
        .callout {{ border-left: 3px solid transparent; }}
        .callout.note {{ border-left-color: #2383e2; }}
        .callout.tip {{ border-left-color: #0f8f5f; }}
        .callout.warning {{ border-left-color: #d9730d; }}
        .callout.important {{ border-left-color: #c0392b; }}
        .task-list-item {{ list-style: none; margin-left: -1.65em; }}
        .task-list-item input {{ margin-right: 8px; accent-color: var(--fg); }}
        #preview-error {{ color: #c4564d; font-size: 14px; margin-top: 12px; }}
        .status {{
            position: fixed; bottom: 24px; left: 50%;
            transform: translateX(-50%);
            font-size: 12px; color: var(--fg-muted);
            background: var(--bg);
            padding: 6px 14px; border-radius: 20px;
            box-shadow: 0 4px 24px rgba(15, 15, 15, 0.08), 0 0 0 1px var(--border);
            opacity: 0; pointer-events: none;
            transition: opacity 0.2s ease;
        }}
        .status.visible {{ opacity: 1; }}
        @media (max-width: 900px) {{
            .page-shell {{ padding: 0 24px 72px; }}
            .page-header {{ padding-top: 48px; }}
            #title {{ font-size: 32px; }}
        }}
        @media (max-width: 768px) {{
            .app {{ flex-direction: column; }}
            .sidebar {{
                width: 100%; min-width: 0; max-height: 200px;
                border-right: none; border-bottom: 1px solid var(--border);
                overflow-y: auto;
            }}
            .sidebar-footer {{ flex-shrink: 0; }}
            .chrome-row {{ flex-wrap: wrap; gap: 8px; }}
            .workbench.live {{ grid-template-columns: 1fr; }}
            .preview-panel {{ border-left: none !important; padding-left: 0 !important; border-top: 1px solid var(--border); padding-top: 24px !important; }}
        }}
    </style>
</head>
<body>
    <div class="app">
        <aside class="sidebar">
            <div class="sidebar-header">Pico Potion</div>
            <form class="new-page-form" method="POST" action="/pages/new">
                <button type="submit" class="new-page-btn">+ 新規ページ</button>
            </form>
            <nav class="page-list" aria-label="ページ一覧">
                <div class="page-list-label">プライベート</div>
                {sidebar}
            </nav>
            <div class="sidebar-footer">
                <form class="delete-form" method="POST" action="/pages/{page_id}/delete" onsubmit="return confirm('このページを削除しますか？');">
                    <button type="submit" class="delete-btn">ページを削除</button>
                </form>
            </div>
        </aside>
        <main class="main">
            <div class="page-shell">
                <header class="page-header">
                    <input id="title" type="text" value="{title_val}" placeholder="無題" aria-label="ページタイトル">
                </header>
                <div id="editor-chrome" class="editor-chrome">
                    <div class="chrome-row">
                        <div class="mode-tabs" role="tablist" aria-label="表示モード">
                            <button type="button" id="tab-edit" class="mode-tab active" role="tab" aria-selected="true">編集</button>
                            <button type="button" id="tab-preview" class="mode-tab" role="tab" aria-selected="false">プレビュー</button>
                        </div>
                        <div class="chrome-actions">
                            <label class="live-toggle">
                                <input type="checkbox" id="live-preview">
                                分割プレビュー
                            </label>
                        </div>
                    </div>
                    <div class="toolbar" aria-label="Markdown ツールバー">
                        <button type="button" class="tool-btn" data-action="bold" title="太字" aria-label="太字">B</button>
                        <button type="button" class="tool-btn" data-action="italic" title="斜体" aria-label="斜体">I</button>
                        <span class="tool-divider" aria-hidden="true"></span>
                        <button type="button" class="tool-btn" data-action="h1" title="見出し1" aria-label="見出し1">H1</button>
                        <button type="button" class="tool-btn" data-action="h2" title="見出し2" aria-label="見出し2">H2</button>
                        <span class="tool-divider" aria-hidden="true"></span>
                        <button type="button" class="tool-btn" data-action="list" title="箇条書き" aria-label="箇条書き">•</button>
                        <button type="button" class="tool-btn" data-action="task" title="タスク" aria-label="タスク">☐</button>
                        <span class="tool-divider" aria-hidden="true"></span>
                        <button type="button" class="tool-btn" data-action="link" title="リンク" aria-label="リンク">↗</button>
                        <button type="button" class="tool-btn" data-action="code" title="コード" aria-label="コード">&lt;/&gt;</button>
                        <button type="button" class="tool-btn" data-action="table" title="表" aria-label="表">▦</button>
                        <button type="button" class="tool-btn" data-action="callout" title="コールアウト" aria-label="コールアウト">!</button>
                        <button type="button" class="tool-btn" data-action="mark" title="ハイライト" aria-label="ハイライト">◐</button>
                        <span class="tool-divider" aria-hidden="true"></span>
                        <button type="button" id="mem-btn" class="tool-btn mem-tool-btn" title="サーバプロセスの RSS（押すと表示）" aria-label="メモリ使用量">MEM</button>
                    </div>
                </div>
                {legacy_hint}
                <div id="workbench" class="workbench">
                    <div id="editor-wrap">
                        <textarea id="md-editor" spellcheck="false" placeholder="ここから入力…  # 見出し、- [ ] タスク、[[ページ名]] など">{content_escaped}</textarea>
                    </div>
                    <div id="preview-wrap" class="hidden">
                        <div class="preview-panel">
                            <div class="preview-tools">
                                <div id="preview-meta" class="preview-meta"></div>
                                <div id="preview-tags" class="preview-meta"></div>
                            </div>
                            <div id="preview-toc"></div>
                            <div id="preview" class="preview-body"></div>
                            <p id="preview-error" class="hidden"></p>
                        </div>
                    </div>
                </div>
                <p class="hint">Markdown: <code># 見出し</code> <code>- [ ] タスク</code> <code>[[ページ名]]</code> <code>&gt; [!NOTE]</code> <code>==強調==</code></p>
            </div>
        </main>
    </div>
    <div id="status" class="status" role="status" aria-live="polite"></div>

    <script>{marked_js}</script>
    <script>
        const MAX_MD_BYTES = 512 * 1024;
        const MARKDOWN_FEATURES = {{
            gfm: true,
            breaks: true,
            callouts: true,
            wikiLinks: true,
            marks: true,
            toc: true,
            tags: true,
            livePreview: true
        }};
        const PAGE_INDEX = {page_index};
        const pageByTitle = Object.create(null);
        PAGE_INDEX.forEach(page => {{ pageByTitle[page.title] = page; }});
        const renderState = {{ headings: [], tags: Object.create(null), links: 0, images: 0, tasks: 0 }};

        function escapeHtml(s) {{
            return String(s || '').replace(/&/g, '&amp;').replace(/</g, '&lt;')
                .replace(/>/g, '&gt;').replace(/"/g, '&quot;');
        }}

        function resetRenderState() {{
            renderState.headings = [];
            renderState.tags = Object.create(null);
            renderState.links = 0;
            renderState.images = 0;
            renderState.tasks = 0;
        }}

        function isSafeUrl(href) {{
            if (!href) return false;
            const value = String(href).trim();
            return /^(https?:|mailto:|tel:)/i.test(value) || value[0] === '/' || value[0] === '#';
        }}

        function isExternalUrl(href) {{
            return /^https?:\/\//i.test(String(href || ''));
        }}

        function slugify(text) {{
            const base = String(text || '').toLowerCase()
                .replace(/<[^>]+>/g, '')
                .replace(/[^\p{{L}}\p{{N}}]+/gu, '-')
                .replace(/^-+|-+$/g, '') || 'section';
            let slug = base;
            let n = 2;
            while (renderState.headings.some(h => h.id === slug)) {{
                slug = base + '-' + n++;
            }}
            return slug;
        }}

        function parseInlineTokens(token) {{
            if (token && token.tokens) return this.parser.parseInline(token.tokens);
            return escapeHtml(token && token.text ? token.text : '');
        }}

        function todayText() {{
            const d = new Date();
            const y = d.getFullYear();
            const m = String(d.getMonth() + 1).padStart(2, '0');
            const day = String(d.getDate()).padStart(2, '0');
            return y + '-' + m + '-' + day;
        }}

        const markdownExtensions = [
            {{
                name: 'mark',
                level: 'inline',
                start(src) {{ return src.indexOf('=='); }},
                tokenizer(src) {{
                    const match = /^==([^=\n][\s\S]*?[^=\n])==/.exec(src);
                    if (!match) return;
                    return {{
                        type: 'mark',
                        raw: match[0],
                        text: match[1],
                        tokens: this.lexer.inlineTokens(match[1])
                    }};
                }},
                renderer(token) {{
                    return '<mark>' + this.parser.parseInline(token.tokens) + '</mark>';
                }}
            }},
            {{
                name: 'wikiLink',
                level: 'inline',
                start(src) {{ return src.indexOf('[['); }},
                tokenizer(src) {{
                    const match = /^\[\[([^\]\n]{{1,80}})\]\]/.exec(src);
                    if (!match) return;
                    return {{ type: 'wikiLink', raw: match[0], text: match[1].trim() }};
                }},
                renderer(token) {{
                    const page = pageByTitle[token.text];
                    const href = page ? '/page/' + encodeURIComponent(page.id) : '/?q=' + encodeURIComponent(token.text);
                    return '<a class="wiki-link" href="' + href + '">' + escapeHtml(token.text) + '</a>';
                }}
            }}
        ];

        marked.use({{
            gfm: true,
            breaks: true,
            silent: true,
            extensions: markdownExtensions,
            hooks: {{
                preprocess(md) {{
                    return String(md || '')
                        .replace(/^[\u200B\u200C\u200D\uFEFF]+/, '')
                        .replace(/^\/1\s+(.+)$/gm, '# $1')
                        .replace(/^\/2\s+(.+)$/gm, '## $1')
                        .replace(/^\/b\s+(.+)$/gm, '- $1')
                        .replace(/^\/todo\s+(.+)$/gm, '- [ ] $1')
                        .replace(/@today\b/g, todayText());
                }},
                postprocess(html) {{
                    return html.replace(/<p>\s*<\/p>/g, '');
                }}
            }},
            renderer: {{
                html(token) {{
                    return escapeHtml(token.text || token.raw || '');
                }},
                link(token) {{
                    const href = token.href || '';
                    const text = parseInlineTokens.call(this, token);
                    if (!isSafeUrl(href)) return text;
                    const title = token.title ? ' title="' + escapeHtml(token.title) + '"' : '';
                    const ext = isExternalUrl(href) ? ' target="_blank" rel="noopener noreferrer"' : '';
                    return '<a href="' + escapeHtml(href) + '"' + title + ext + '>' + text + '</a>';
                }},
                image(token) {{
                    const href = token.href || '';
                    if (!isSafeUrl(href)) return '';
                    const title = token.title ? ' title="' + escapeHtml(token.title) + '"' : '';
                    return '<img src="' + escapeHtml(href) + '" alt="' + escapeHtml(token.text || '') + '"' + title + ' loading="lazy">';
                }},
                heading(token) {{
                    const text = parseInlineTokens.call(this, token);
                    const plain = token.text || text.replace(/<[^>]+>/g, '');
                    const id = slugify(plain);
                    renderState.headings.push({{ id, text: plain, depth: token.depth || 1 }});
                    return '<h' + token.depth + ' id="' + id + '">' + text + '</h' + token.depth + '>';
                }},
                table(token) {{
                    const alignAttr = align => align ? ' style="text-align:' + escapeHtml(align) + '"' : '';
                    const cellHtml = (cell, tag, index) => {{
                        const text = cell.tokens ? this.parser.parseInline(cell.tokens) : escapeHtml(cell.text || '');
                        const align = cell.align || (token.align && token.align[index]) || '';
                        return '<' + tag + alignAttr(align) + '>' + text + '</' + tag + '>';
                    }};
                    const head = '<thead><tr>' + token.header.map((cell, index) => cellHtml(cell, 'th', index)).join('') + '</tr></thead>';
                    const body = '<tbody>' + token.rows.map(row => '<tr>' + row.map((cell, index) => cellHtml(cell, 'td', index)).join('') + '</tr>').join('') + '</tbody>';
                    return '<div class="table-wrap"><table>' + head + body + '</table></div>';
                }},
                listitem(token) {{
                    if (!token.task) return false;
                    const checked = token.checked ? ' checked' : '';
                    const body = token.tokens ? this.parser.parse(token.tokens) : escapeHtml(token.text || '');
                    return '<li class="task-list-item"><input type="checkbox"' + checked + '> ' + body + '</li>';
                }},
                code(token) {{
                    const lang = (token.lang || '').split(/\s+/)[0].replace(/[^\w-]/g, '');
                    const label = lang || 'text';
                    const cls = lang ? ' class="language-' + escapeHtml(lang) + '"' : '';
                    return '<div class="code-block"><div class="code-head"><span>' + escapeHtml(label) + '</span><button type="button" class="copy-code">Copy</button></div><pre><code' + cls + '>' + escapeHtml(token.text || '') + '</code></pre></div>';
                }},
                blockquote(token) {{
                    const body = this.parser.parse(token.tokens || []);
                    const match = body.match(/^<p>\s*\[!(NOTE|TIP|WARNING|IMPORTANT)\]\s*(?:<br\s*\/?>)?\s*/i);
                    if (!match) return '<blockquote>' + body + '</blockquote>';
                    const type = match[1].toLowerCase();
                    const title = match[1].charAt(0).toUpperCase() + match[1].slice(1).toLowerCase();
                    const rest = body.replace(/^<p>\s*\[!(NOTE|TIP|WARNING|IMPORTANT)\]\s*(?:<br\s*\/?>)?\s*/i, '<p>').replace(/^<p>\s*<\/p>\s*/, '');
                    return '<div class="callout ' + type + '"><div class="callout-title">' + title + '</div>' + rest + '</div>';
                }}
            }},
            walkTokens(token) {{
                if (token.type === 'link') renderState.links++;
                if (token.type === 'image') renderState.images++;
                if (token.type === 'list_item' && token.task) renderState.tasks++;
                if (token.type === 'text') {{
                    const re = /(^|[\s　])#([A-Za-z0-9_\-\u3040-\u30ff\u3400-\u9fff]+)/g;
                    let m;
                    while ((m = re.exec(token.text)) !== null) {{
                        renderState.tags[m[2]] = true;
                    }}
                }}
            }}
        }});

        const PAGE_ID = "{page_id_js}";
        const mdEditor = document.getElementById('md-editor');
        const titleInput = document.getElementById('title');
        const status = document.getElementById('status');
        const workbench = document.getElementById('workbench');
        const editorWrap = document.getElementById('editor-wrap');
        const previewWrap = document.getElementById('preview-wrap');
        const preview = document.getElementById('preview');
        const previewError = document.getElementById('preview-error');
        const previewMeta = document.getElementById('preview-meta');
        const previewTags = document.getElementById('preview-tags');
        const previewToc = document.getElementById('preview-toc');
        const tabEdit = document.getElementById('tab-edit');
        const tabPreview = document.getElementById('tab-preview');
        const livePreview = document.getElementById('live-preview');
        const editorChrome = document.getElementById('editor-chrome');
        const mainEl = document.querySelector('.main');
        const SAVE_INTERVAL_MS = 10000;
        let isDirty = false;
        let isSaving = false;
        let previewTimeout = null;
        let statusHideTimeout = null;

        function setTabState(editActive) {{
            tabEdit.classList.toggle('active', editActive);
            tabPreview.classList.toggle('active', !editActive);
            tabEdit.setAttribute('aria-selected', editActive ? 'true' : 'false');
            tabPreview.setAttribute('aria-selected', editActive ? 'false' : 'true');
        }}

        function flashStatus(text) {{
            status.innerText = text;
            status.classList.add('visible');
            clearTimeout(statusHideTimeout);
            statusHideTimeout = setTimeout(() => status.classList.remove('visible'), 2200);
        }}

        if (mainEl) {{
            mainEl.addEventListener('scroll', () => {{
                editorChrome.classList.toggle('is-scrolled', mainEl.scrollTop > 8);
            }}, {{ passive: true }});
        }}

        function showEdit() {{
            livePreview.checked = false;
            workbench.classList.remove('live');
            editorWrap.classList.remove('hidden');
            previewWrap.classList.add('hidden');
            setTabState(true);
        }}

        function showPreview() {{
            livePreview.checked = false;
            workbench.classList.remove('live');
            editorWrap.classList.add('hidden');
            previewWrap.classList.remove('hidden');
            setTabState(false);
            renderMarkdown();
        }}

        function setPreviewError(message) {{
            preview.innerHTML = '';
            previewMeta.innerHTML = '';
            previewTags.innerHTML = '';
            previewToc.innerHTML = '';
            previewError.textContent = message;
            previewError.classList.remove('hidden');
        }}

        function renderMeta() {{
            previewMeta.innerHTML =
                '<span class="meta-pill">見出し ' + renderState.headings.length + '</span>' +
                '<span class="meta-pill">リンク ' + renderState.links + '</span>' +
                '<span class="meta-pill">画像 ' + renderState.images + '</span>' +
                '<span class="meta-pill">タスク ' + renderState.tasks + '</span>';
            const tags = Object.keys(renderState.tags).sort();
            previewTags.innerHTML = tags.map(tag => '<span class="tag-pill">#' + escapeHtml(tag) + '</span>').join('');
        }}

        function renderToc() {{
            if (!renderState.headings.length) {{
                previewToc.innerHTML = '';
                return;
            }}
            const links = renderState.headings.map(h =>
                '<a class="toc-depth-' + h.depth + '" href="' + '#' + h.id + '">' + escapeHtml(h.text) + '</a>'
            ).join('');
            previewToc.innerHTML = '<nav class="toc"><div class="toc-title">目次</div>' + links + '</nav>';
        }}

        function renderMarkdown() {{
            previewError.classList.add('hidden');
            previewError.textContent = '';
            if (mdEditor.value.length > MAX_MD_BYTES) {{
                setPreviewError('本文が長すぎます（上限512KB）');
                return;
            }}

            try {{
                resetRenderState();
                preview.innerHTML = marked.parse(mdEditor.value);
                renderMeta();
                renderToc();
                bindRenderedControls();
            }} catch (_) {{
                setPreviewError('プレビューできませんでした');
            }}
        }}

        function schedulePreview() {{
            if (previewWrap.classList.contains('hidden')) return;
            clearTimeout(previewTimeout);
            previewTimeout = setTimeout(renderMarkdown, 350);
        }}

        function setLinePrefix(prefix) {{
            const start = mdEditor.selectionStart;
            const lineStart = mdEditor.value.lastIndexOf('\n', start - 1) + 1;
            mdEditor.setRangeText(prefix, lineStart, lineStart, 'end');
        }}

        function wrapSelection(before, after, placeholder) {{
            const start = mdEditor.selectionStart;
            const end = mdEditor.selectionEnd;
            const selected = mdEditor.value.slice(start, end) || placeholder;
            mdEditor.setRangeText(before + selected + after, start, end, 'select');
        }}

        function insertBlock(text) {{
            const start = mdEditor.selectionStart;
            const needsBreak = start > 0 && mdEditor.value[start - 1] !== '\n';
            mdEditor.setRangeText((needsBreak ? '\n' : '') + text, start, mdEditor.selectionEnd, 'end');
        }}

        function applyTool(action) {{
            mdEditor.focus();
            if (action === 'bold') wrapSelection('**', '**', '太字');
            if (action === 'italic') wrapSelection('*', '*', '斜体');
            if (action === 'h1') setLinePrefix('# ');
            if (action === 'h2') setLinePrefix('## ');
            if (action === 'list') setLinePrefix('- ');
            if (action === 'task') setLinePrefix('- [ ] ');
            if (action === 'link') wrapSelection('[', '](https://example.com)', 'リンク');
            if (action === 'code') wrapSelection('`', '`', 'code');
            if (action === 'table') insertBlock('| 項目 | 内容 |\n| --- | --- |\n| 例 | メモ |\n');
            if (action === 'callout') insertBlock('> [!NOTE]\n> メモ\n');
            if (action === 'mark') wrapSelection('==', '==', '強調');
            markDirty();
            schedulePreview();
        }}

        function toggleTaskByIndex(taskIndex, checked) {{
            let seen = -1;
            const lines = mdEditor.value.split('\n');
            for (let i = 0; i < lines.length; i++) {{
                if (/^\s*[-*+]\s+\[[ xX]\]\s+/.test(lines[i])) {{
                    seen++;
                    if (seen === taskIndex) {{
                        lines[i] = lines[i].replace(/\[[ xX]\]/, checked ? '[x]' : '[ ]');
                        mdEditor.value = lines.join('\n');
                        markDirty();
                        schedulePreview();
                        return;
                    }}
                }}
            }}
        }}

        function bindRenderedControls() {{
            preview.querySelectorAll('.copy-code').forEach(btn => {{
                btn.addEventListener('click', () => {{
                    const code = btn.closest('.code-block').querySelector('code');
                    if (navigator.clipboard && code) navigator.clipboard.writeText(code.textContent);
                }});
            }});
            preview.querySelectorAll('input[type="checkbox"]').forEach((box, index) => {{
                box.addEventListener('change', () => toggleTaskByIndex(index, box.checked));
            }});
        }}

        tabEdit.addEventListener('click', showEdit);
        tabPreview.addEventListener('click', showPreview);
        livePreview.addEventListener('change', () => {{
            if (livePreview.checked) {{
                workbench.classList.add('live');
                editorWrap.classList.remove('hidden');
                previewWrap.classList.remove('hidden');
                setTabState(true);
                tabPreview.classList.add('active');
                renderMarkdown();
            }} else {{
                showEdit();
            }}
        }});
        document.querySelectorAll('.tool-btn').forEach(btn => {{
            btn.addEventListener('click', () => applyTool(btn.dataset.action));
        }});

        mdEditor.addEventListener('input', markDirty);
        mdEditor.addEventListener('input', schedulePreview);
        titleInput.addEventListener('input', markDirty);

        function markDirty() {{
            isDirty = true;
        }}

        function buildSaveBody() {{
            const params = new URLSearchParams();
            params.append('id', PAGE_ID);
            params.append('title', titleInput.value);
            params.append('content', mdEditor.value);
            return params;
        }}

        function saveData() {{
            if (!isDirty || isSaving) return;
            isSaving = true;
            flashStatus('保存中…');
            const body = buildSaveBody();

            fetch('/save', {{
                method: 'POST',
                headers: {{ 'Content-Type': 'application/x-www-form-urlencoded' }},
                body
            }})
            .then(res => {{
                if (res.ok) {{
                    isDirty = false;
                    flashStatus('保存しました');
                }} else {{
                    flashStatus('保存に失敗しました');
                }}
            }})
            .catch(() => {{
                flashStatus('保存に失敗しました');
            }})
            .finally(() => {{
                isSaving = false;
            }});
        }}

        function flushSave() {{
            if (!isDirty) return;
            const body = buildSaveBody();
            fetch('/save', {{
                method: 'POST',
                headers: {{ 'Content-Type': 'application/x-www-form-urlencoded' }},
                body,
                keepalive: true
            }});
            isDirty = false;
        }}

        setInterval(saveData, SAVE_INTERVAL_MS);
        document.addEventListener('visibilitychange', () => {{
            if (document.visibilityState === 'hidden') flushSave();
        }});
        window.addEventListener('pagehide', flushSave);

        const memBtn = document.getElementById('mem-btn');
        if (memBtn) {{
            memBtn.addEventListener('click', () => {{
                memBtn.disabled = true;
                flashStatus('メモリ取得中…');
                fetch('/stats/memory')
                    .then(res => {{
                        if (!res.ok) throw new Error('stats');
                        return res.json();
                    }})
                    .then(data => {{
                        const mb = typeof data.rss_mb === 'number' ? data.rss_mb : data.rss_kb / 1024;
                        flashStatus('RSS: ' + mb.toFixed(2) + ' MB');
                    }})
                    .catch(() => {{
                        flashStatus('メモリ取得に失敗');
                    }})
                    .finally(() => {{
                        memBtn.disabled = false;
                    }});
            }});
        }}
    </script>
</body>
</html>"#,
        page_title = escape_html(title),
        marked_js = MARKED_JS,
        page_index = page_index,
        sidebar = sidebar,
        page_id = escape_html(page_id),
        title_val = escape_html(title),
        content_escaped = escape_html(content),
        legacy_hint = legacy_hint,
        page_id_js = escape_html(page_id),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_index_json_escapes_titles_and_ids() {
        let pages = vec![
            PageMeta {
                id: "home".to_string(),
                title: "家族の共有ノート".to_string(),
            },
            PageMeta {
                id: "p\"<>&".to_string(),
                title: "A \"quoted\" <page> & note".to_string(),
            },
        ];

        let json = render_page_index_json(&pages);

        assert!(json.contains(r#"{"id":"home","title":"家族の共有ノート"}"#));
        assert!(json.contains(
            r#"{"id":"p\"\u003C\u003E\u0026","title":"A \"quoted\" \u003Cpage\u003E \u0026 note"}"#
        ));
        assert!(!json.contains("</script>"));
    }

    #[test]
    fn parse_vm_rss_kib_reads_proc_status() {
        let sample = "Name:\tpico_potion\nVmRSS:\t  2048 kB\n";
        assert_eq!(parse_vm_rss_kib(sample), Some(2048));
    }

    #[test]
    fn html_template_embeds_marked_full_use_features() {
        let pages = vec![PageMeta {
            id: "home".to_string(),
            title: "Home".to_string(),
        }];

        let html = get_html_template("home", "Home", "# Hello", &pages);

        assert!(html.contains("const MARKDOWN_FEATURES"));
        assert!(html.contains("const PAGE_INDEX"));
        assert!(html.contains("gfm: true"));
        assert!(html.contains("breaks: true"));
        assert!(html.contains("walkTokens"));
        assert!(html.contains("name: 'mark'"));
        assert!(html.contains("name: 'wikiLink'"));
        assert!(html.contains("data-action=\"bold\""));
        assert!(html.contains("id=\"live-preview\""));
        assert!(html.contains("id=\"mem-btn\""));
        assert!(html.contains(">MEM</button>"));
        assert!(html.contains("/stats/memory"));
    }
}
