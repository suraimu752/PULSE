# AGENTS.md

PULSE を変更するエージェント向けの作業規則です。仕様の正本はオーナーの開発指示書と `docs/` です。

## 言語

- ソースとコメントは英語
- ユーザーに見える文言（UI、README、エラーダイアログ）は日本語
- 計測できない値を `0` と表示しない。`N/A`（計測不能）と `未対応`（未実装）を分ける
- ダミーの CPU・メモリ・転送量を本番の数値として出さない

## ブランチ

- デフォルトブランチへ直接 push しない
- GitHub Release を作らない（Phase 1 の配布は Actions の `PULSE.exe` アーティファクト）
- `FUNDING.yml` に仮の寄付先を書かない
- 既存の変更を消す強制 push や履歴の書き換えをしない

## レイヤ

| レイヤ | 場所 | 役割 |
| --- | --- | --- |
| UI | `ui/app.slint`, `src/ui.rs`, `src/present.rs` | 表示、ソート、グラフ。Windows API を呼ばない |
| Sampler | `src/sampler.rs` | 計測スレッド、公開スナップショット、リングバッファ |
| Aggregator | `src/aggregate.rs` | 区間の使用率と速度 |
| Collector | `src/collector.rs`, `src/platform/` | OS の読み取り |
| History | `src/history.rs` | 固定長リングバッファ |
| Model | `src/model.rs`, `src/metric.rs` | 層の間で渡す型 |

Collector を足すときは `MetricEnricher` を実装する。UI の列はすでにあり、`MetricValue::Unsupported` のままなら `未対応` と出る。

## 計測

- 間隔の定数は `src/config.rs`。表示更新と計測間隔は別
- 履歴は全プロセスを保持する。保存時点で上位 5 件だけにする実装は禁止
- 過去スナップショットを、いまの値で上書きしない
- プロセス識別は PID に加え、取れた場合は作成 FILETIME
- 新しい Windows API は Microsoft Learn の説明を優先し、採用理由を `docs/decisions.md` に残す

## 検証

ターゲットは Windows 11 x86-64。開発マシンが Linux でも、次はローカルで実行する。

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo clippy --target x86_64-pc-windows-gnu --no-default-features --all-targets -- -D warnings
```

`PULSE.exe` の起動確認は Windows 実機が必要。Linux 上の起動成功をもって Windows で動くと書かない。CI は `.github/workflows/ci.yml`。`windows-latest` が fmt、clippy、test、release ビルド、`PULSE-windows-x64` アーティファクトを担当する。

## ライセンス

プロジェクトは GPL-3.0-only。Slint は三つのライセンスから選べるが、このリポジトリでは GPL-3.0-only だけを選ぶ。依存を足す前にライセンスが GPL-3.0-only と組み合わせられることを `docs/licenses.md` に記録する。
