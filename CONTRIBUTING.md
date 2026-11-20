# 貢献ガイド

## 開発の流れ

1. `main` からブランチを切る
2. 変更を小さく保ち、`cargo fmt`、`cargo clippy --all-targets --all-features -- -D warnings`、`cargo test --all-targets --all-features` を通す
3. プルリクエストを `main` 向けに開く。`main` へ直接 push しない
4. GitHub Release は、配布手順が `docs/` に揃うまで作らない

## コード

- Rust stable。エディションは 2021
- コメントと識別子は英語。画面に出る文は日本語
- 外部クレートは必要最小限。追加するときはライセンス、メンテナンス、バイナリサイズを `docs/licenses.md` に書く
- Windows API は `src/platform/windows.rs` に閉じる
- 未実装の計測は `MetricValue::Unsupported` のまま返す。`0` で埋めない

## テスト

計算、ソート、リングバッファ、表示文言は OS に依存しないテストにする。Windows API を呼ぶテストは `#[cfg(windows)]` に置き、CI の `windows-latest` で実行する。

## 報告

不具合は GitHub Issues へ。計測値のずれを報告するときは、タスクマネージャーのどの列と比べたか（ワーキングセットか、アクティブプライベートワーキングセットか）を書いてください。定義は `docs/measurement.md` にあります。
