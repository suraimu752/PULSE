# 変更履歴

このファイルは [Keep a Changelog](https://keepachangelog.com/ja/1.1.0/) に近づけて書く。バージョンは SemVer。

## [0.1.0] - Unreleased

GitHub Release は作成していない。実行ファイルは GitHub Actions のアーティファクト `PULSE-windows-x64` にある。

### 追加

- Slint による Windows 11 風の基本ウィンドウ（プロセス、パフォーマンス。他のナビ項目は準備中）
- プロセス一覧の縦横スクロールバー。列幅はヘッダー境界のドラッグで変更でき、横スクロール範囲は列幅の合計に従う
- 左ナビを独自アイコンにし、ハンバーガーでアイコンのみのレールに折りたためる
- パフォーマンスの資源リストと詳細、はみ出した左ナビにも縦スクロールバー。一覧はウィンドウ高さに固定し、中身が長いときだけスクロールする
- 各資源の簡易グラフ、60 秒のメイングラフ、現在の上位 5 プロセス
- 開発専用 `mock` feature（`DEMO DATA`）。通常の Release ビルドでは使えない
- ヘッドレス UI テスト（`i-slint-backend-testing`）。1280×720、1920×1080、800×600、ライトとダーク
- ライト / ダーク / システムの表示切替。システムは Windows の `AppsUseLightTheme` に従う
- 開発用のデザインスキル（impeccable、taste-skill、emil-design-eng）を `.cursor/skills/` に同梱。実行ファイルにはリンクしない
- プロセス一覧、CPU 使用率、プライベートワーキングセットの実測
- 列ソート。`N/A` と `未対応` は数値の 0 と分け、ソートでも末尾に置く
- 計測スレッドと UI スレッドの分離
- 直近 60 秒・500 ms 間隔の固定長リングバッファ（120 個、全プロセス）
- CPU とメモリの 60 秒グラフ。ディスク、ネットワーク、GPU、VRAM は `未対応`
- Windows 向け CI（fmt、clippy、test、release ビルド、`PULSE.exe` のアップロード）
