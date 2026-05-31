use axum::{
    extract::{Form, State},
    response::Html,
    routing::{get, post},
    Router,
};
use rusqlite::Connection;
use std::sync::{Arc, Mutex};

// スレッド間で安全にSQLiteの接続を共有するための型定義
type DbState = Arc<Mutex<Connection>>;

#[derive(serde::Deserialize)]
struct SaveForm {
    content: String,
}

#[tokio::main]
async fn main() {
    // 1. データベースの初期化 (単一ファイル、または ":memory:" で完全インメモリ化も可能)
    let conn = Connection::open("micro_notion.db").expect("Failed to open DB");
    conn.execute(
        "CREATE TABLE IF NOT EXISTS pages (id TEXT PRIMARY KEY, content TEXT)",
        [],
    )
    .expect("Failed to create table");
    
    let db_state = Arc::new(Mutex::new(conn));

    // 2. ルーティングの設定
    let app = Router::new()
        .route("/", get(handle_index))
        .route("/save", post(handle_save))
        .with_state(db_state);

    // 3. ラズパイ上のすべてのネットワークインターフェースで受付け (ポート 8080)
    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await.unwrap();
    println!("🚀 Ultra Light Notion started on http://localhost:8080");
    axum::serve(listener, app).await.unwrap();
}

// 画面表示（HTMLをそのまま返す）
async fn handle_index(State(db): State<DbState>) -> Html<String> {
    let conn = db.lock().unwrap();
    let mut stmt = conn.prepare("SELECT content FROM pages WHERE id = 'home'").unwrap();
    let content: String = stmt
        .query_row([], |row| row.get(0))
        .unwrap_or_else(|_| "".to_string());

    // 下記で定義するHTMLテンプレートを注入
    Html(get_html_template(&content))
}

// 自動保存の受付
async fn handle_save(State(db): State<DbState>, Form(form): Form<SaveForm>) {
    let conn = db.lock().unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO pages (id, content) VALUES ('home', ?1)",
        [&form.content],
    )
    .unwrap();
}

// フロントエンドのソースコードを1つの文字列として内蔵（省メモリ・ファイル読み込みIOの削減）
fn get_html_template(content: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="ja">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Micro Notion (Rust)</title>
    <style>
        body {{ font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Helvetica, Arial, sans-serif; max-width: 700px; margin: 40px auto; padding: 0 20px; color: #333; background-color: #fff; }}
        #editor {{ min-height: 500px; outline: none; font-size: 16px; line-height: 1.8; color: #222; }}
        h1 {{ font-size: 2em; border-bottom: 1px solid #eee; padding-bottom: 0.3em; }}
        h2 {{ font-size: 1.5em; margin-top: 1.5em; }}
        ul {{ padding-left: 20px; }}
        .hint {{ color: #999; font-size: 13px; margin-bottom: 30px; border-left: 3px solid #ddd; padding-left: 10px; }}
        .status {{ position: fixed; bottom: 20px; right: 20px; font-size: 12px; color: #aaa; background: #fafafa; padding: 4px 8px; border-radius: 4px; border: 1px solid #eee; }}
    </style>
</head>
<body>
    <h1>📋 家族の共有ノート</h1>
    <div class="hint">ショートカット: 「<b>/1 </b>」大見出し / 「<b>/2 </b>」小見出し / 「<b>/b </b>」箇条書きリスト (最後に半角スペース)</div>
    
    <div id="editor" contenteditable="true">{}</div>
    <div id="status" class="status">保存済み</div>

    <script>
        const editor = document.getElementById('editor');
        const status = document.getElementById('status');
        let timeout = null;

        // Notion風のスラッシュコマンドの実装
        editor.addEventListener('input', () => {{
            let html = editor.innerHTML;
            let changed = false;

            if (html.includes('/1&nbsp;')) {{
                document.execCommand('formatBlock', false, '<h1>');
                editor.innerHTML = editor.innerHTML.replace('/1&nbsp;', '');
                changed = true;
            }} else if (html.includes('/2&nbsp;')) {{
                document.execCommand('formatBlock', false, '<h2>');
                editor.innerHTML = editor.innerHTML.replace('/2&nbsp;', '');
                changed = true;
            }} else if (html.includes('/b&nbsp;')) {{
                document.execCommand('insertUnorderedList', false, null);
                editor.innerHTML = editor.innerHTML.replace('/b&nbsp;', '');
                changed = true;
            }}

            status.innerText = "入力中...";
            
            // タイピングが止まって500ms後に自動保存 (デバウンス)
            clearTimeout(timeout);
            timeout = setTimeout(saveData, 500);
        }});

        function saveData() {{
            const params = new URLSearchParams();
            params.append('content', editor.innerHTML);

            fetch('/save', {{
                method: 'POST',
                headers: {{ 'Content-Type': 'application/x-www-form-urlencoded' }},
                body: params
            }})
            .then(res => {{
                if(res.ok) status.innerText = "変更を保存しました";
            }})
            .catch(() => {{
                status.innerText = "⚠️ 保存失敗 (LAN切断？)";
            }});
        }}
    </script>
</body>
</html>"#,
        content
    )
}