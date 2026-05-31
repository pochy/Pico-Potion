# Micro Notion (Rust)

家庭内LAN環境のRaspberry Pi上で動作する、極限まで軽量化されたセルフホスト型のNotion風Webアプリケーションです。

## 📋 背景と目的

家庭内LANで家族間の情報共有（メモ、買い物リスト、回覧板など）やシステム通知の集約を行うにあたり、既存のOSS（Mattermost、Affineなど）は高機能な反面、メモリ消費量が数百MB〜数GBに達し、シングルボードコンピュータであるRaspberry Piのリソースを大きく圧迫するという課題がありました。

本プロジェクトは、**「常時稼働中の消費メモリを5MB以下に抑える」**ことを目的に設計されています。機能を本質的なもの（テキスト編集、自動保存、簡易装飾）に絞り込み、言語やアーキテクチャを最適化することで、ラズパイの負荷をほぼゼロにしつつ、実用的な共有ノート環境を提供します。

## 🛠️ 使用技術と省メモリ戦略

### バックエンド (Rust / Axum)

* **Rust言語の採用:** ガベージコレクション（GC）を持たないRustを採用し、使い終わったメモリを即座に解放。待機時メモリを1.5MB〜2MB程度に抑制。
* **Axum:** 高速かつ超軽量なマクロベースのWebフレームワークを採用。
* **SQLite (rusqlite):** 外部データベースプロセス（PostgreSQL等）を立ち上げず、アプリプロセス内で単一ファイルとしてデータを管理することで、データベース起因のメモリ消費を完全に排除。

### フロントエンド (Vanilla JS / HTML5)

* **ライブラリフリー:** React、Vue、Editor.jsなどの外部フレームワーク・エディタライブラリを一切排除。
* **標準機能の活用:** HTML5の `contenteditable` 属性と生JavaScript（Vanilla JS）のみでNotion風のスラッシュコマンド（`/1`, `/2`, `/b`）および自動保存（デバウンス処理）を実現。
* **シングルファイル化:** CSSやJSを1枚のHTMLに内蔵し、Rustのバイナリ（文字列型）に組み込むことで、ファイルI/Oのオーバーヘッドを削減。

## 🚀 使い方

### 1. ビルド

ラズパイ上でリリースモードでコンパイルします。

```bash
cargo build --release
```

バイナリは `target/release/micro_notion` に出力されます。

> **注意:** Mac や Windows でビルドしたバイナリは、そのままラズパイ（Linux aarch64）では動きません。ラズパイ上でビルドするか、クロスコンパイルが必要です。

### 2. 手動起動（動作確認用）

```bash
./target/release/micro_notion
```

起動後、同じマシンから `http://localhost:8080` で確認できます。

### 3. アクセス

同じLAN内のMacやWindowsのブラウザから以下のアドレスにアクセスします。

```text
http://<ラズパイのIPアドレス>:8080
```

### 4. ショートカット

エディタ内で以下の文字を入力した直後に**半角スペース**を入力すると、Notion風にブロックが変換されます。

* `/1` : 大見出し (H1)
* `/2` : 小見出し (H2)
* `/b` : 箇条書きリスト (UL/LI)

---

## 🔧 Raspberry Pi 上で systemd サービスとして常駐させる

Raspberry Pi OS（Debian）は **systemd** が標準です。`nohup` や手動起動より、systemd に登録すると以下のメリットがあります。

* 電源投入後の自動起動
* クラッシュ時の自動再起動
* `journalctl` によるログ管理

### 前提: Rust ツールチェーンのインストール（初回のみ）

ラズパイ上でまだ Rust を入れていない場合:

```bash
sudo apt update
sudo apt install -y build-essential pkg-config libssl-dev
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env
```

### 1. ビルド

プロジェクトディレクトリで:

```bash
cargo build --release
```

### 2. 配置

DB ファイル（`micro_notion.db`）は**カレントディレクトリ**に作成されるため、systemd の `WorkingDirectory` を固定して運用します。

```bash
sudo useradd --system --no-create-home --shell /usr/sbin/nologin micro-notion

sudo mkdir -p /opt/micro-notion
sudo cp target/release/micro_notion /opt/micro-notion/
sudo chown -R micro-notion:micro-notion /opt/micro-notion
sudo chmod 755 /opt/micro-notion/micro_notion
```

初回起動後、`/opt/micro-notion/micro_notion.db` が自動作成されます。

### 3. systemd ユニットファイルの作成

```bash
sudo nano /etc/systemd/system/micro-notion.service
```

以下を貼り付けます:

```ini
[Unit]
Description=Micro Notion (family shared note)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=micro-notion
Group=micro-notion
WorkingDirectory=/opt/micro-notion
ExecStart=/opt/micro-notion/micro_notion

Restart=on-failure
RestartSec=5

# セキュリティ（任意だが推奨）
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/opt/micro-notion

[Install]
WantedBy=multi-user.target
```

### 4. 有効化・起動

```bash
sudo systemctl daemon-reload
sudo systemctl enable micro-notion
sudo systemctl start micro-notion
sudo systemctl status micro-notion
```

正常なら `Active: active (running)` と表示されます。

### 5. 動作確認

```bash
curl http://localhost:8080
```

LAN 内の他端末からは `http://<ラズパイのIPアドレス>:8080` でアクセスできます（`0.0.0.0:8080` で待ち受け）。

### よく使うコマンド

| 操作 | コマンド |
|------|----------|
| 状態確認 | `sudo systemctl status micro-notion` |
| 停止 | `sudo systemctl stop micro-notion` |
| 再起動 | `sudo systemctl restart micro-notion` |
| 自動起動 OFF | `sudo systemctl disable micro-notion` |
| ログ（リアルタイム） | `journalctl -u micro-notion -f` |
| ログ（今日分） | `journalctl -u micro-notion --since today` |

### バイナリ更新時

新しいバイナリを配置してから再起動します。

```bash
sudo cp target/release/micro_notion /opt/micro-notion/
sudo chown micro-notion:micro-notion /opt/micro-notion/micro_notion
sudo systemctl restart micro-notion
```

### 補足

* **systemd とデーモン:** 「デーモン」はバックグラウンド常駐プロセスの総称。Linux では systemd がそれを管理するのが一般的です。
* **ポート:** 現在は `8080` 固定です（`src/main.rs` 内で指定）。
* **80番ポートで公開したい場合:** 1024 未満のポートは root 権限が必要なため、nginx 等でリバースプロキシするのが一般的です。

---

## 📦 コマンド一発で Zip にまとめる方法

ファイル（`Cargo.toml`, `src/main.rs`, `README.md`）が揃ったら、ターミナルで以下のコマンドを実行してください。プロジェクト一式を `micro_notion.zip` に圧縮します。

### Mac / Linux（ラズパイ）の場合

```bash
zip -r micro_notion.zip Cargo.toml src/ README.md

```

### Windows (PowerShell) の場合

```powershell
Compress-Archive -Path Cargo.toml, src, README.md -DestinationPath micro_notion.zip

```

