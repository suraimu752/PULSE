# PULSE

Process Usage and Load Surveillance Engine。Windows 11 向けの軽量タスクマネージャーです。

「いつ、どのプロセスが、どのリソースを使ったか」を後から追えることを目標にしています。Phase 1 は、そのための計測と表示の土台です。

## スクリーンショット

Linux 上のレイアウト確認です。`--features mock` の DEMO DATA で、Windows 11 の実測ではありません。

![パフォーマンス 1280×720 ライト](docs/screenshots/perf-light-1280x720.png)

![パフォーマンス 1280×720 ダーク](docs/screenshots/perf-dark-1280x720.png)

![パフォーマンス 1920×1080 ライト](docs/screenshots/perf-light-1920x1080.png)

![パフォーマンス 800×600 ライト](docs/screenshots/perf-light-800x600.png)

![30秒前の CPU を固定（ライト 1280×720、DEMO DATA）](docs/screenshots/history-cpu-light-1280.png)

## デモデータで起動する（Linux）

通常の Release ビルドには入っていません。

```bash
cargo run --features mock -- --demo normal --page performance --size 1280x720 --theme light
```

シナリオは `normal`（100 プロセス）、`many`（1,000 プロセス）、`gaps`（N/A・未対応・実測 0）、`special`（出現、終了、PID 再利用、複数 GPU / ディスク / アダプター）、`history`（30 秒前の CPU スパイク、終了したプロセス、PID 再利用、2 つの GPU）です。画面に `DEMO DATA` と出ます。乱数ではなく決まった式です。

```bash
cargo run --features mock -- --demo history --page performance --size 1280x720 --theme dark --select-history 59
```

`--resource` は `cpu` / `memory` / `disk` / `network` / `gpu3d` / `gpu-encode` / `gpu-decode` / `vram` / `shared` です。`--select-history N` は 60 秒軸の N 番目の棒を一度だけ固定します。どちらも画面確認用です。

## 主な機能（Phase 1）

- Windows 11 のタスクマネージャーに近い、左ナビゲーションと右メイン領域。左上のハンバーガーでアイコンだけの幅に折りたためる
- プロセスタブ: 実在のプロセス一覧、CPU 使用率、プライベートワーキングセット。縦横のスクロールバー付き
- 列ヘッダーのクリックで昇順・降順ソート。ヘッダーの右端をドラッグすると列幅が変わる。ヘッダー本体のドラッグ、または「列」メニューで並べ替えと表示切替ができる。名前列は常に表示
- パフォーマンスの資源も「項目」メニューで並べ替えと表示切替ができる。一覧と詳細の境目をドラッグして幅を変える
- 表示はライト、ダーク、または Windows のアプリテーマ（システム）。切替は左ナビの下
- パフォーマンスタブ: 資源一覧、直近 60 秒のグラフ、その時点の上位 5 プロセス。グラフの棒をクリックするとその時点を固定し、「ライブに戻る」で最新に戻る。ホバーは時刻と値のプレビューだけ
- 計測スレッドと UI スレッドの分離
- 直近 60 秒分のスナップショットを固定長リングバッファに保持（全プロセス。上位 N 件だけは保存しない）
- プロセス別の I/O 速度と、それとは別の物理ディスク速度。プロセス別ネットワークは通常ユーザーでは `要管理者`。アダプター速度は別
- GPU 3D / Encode / Decode（最大。合計しない）、専用 VRAM と共有 GPU メモリ
- 計測できない値は `N/A`。GPU が無いときは `なし`。`0` や `0.0%` は実測がゼロのときだけ

列と資源の並び、非表示、分割位置は `%APPDATA%\PULSE\ui.cfg` に保存する。Linux では `~/.config/PULSE/ui.cfg`。環境変数 `PULSE_CONFIG_DIR` があればそのディレクトリを使う。ネットワークへは送らない。

検索と論理プロセッサ別ランキングは未実装です。ナビゲーションの「準備中」項目はその置き場です。

## 対応 OS

- Windows 11 x86-64
- 通常のユーザー権限で起動する（常時の管理者権限は不要）
- 一部のプロセスは権限不足で CPU / メモリが `N/A` になる

Linux の通常ビルドは計測値を出さない。`mock` feature の `DEMO DATA` は計測結果ではない。

## インストール

安定版の GitHub Release は Phase 1 では作っていません。

Windows 用の `PULSE.exe` は、このリポジトリの GitHub Actions（`windows-latest`）がリリースビルドした成果物です。

1. Actions の成功した `fmt / clippy / test / build (Windows)` を開く
2. Artifacts の `PULSE-windows-x64` をダウンロードする
3. ZIP 内の `PULSE.exe` を Windows 11 x86-64 上で実行する

アーティファクトは GitHub の保存期限があるため、期限後は Actions を再実行するか、ソースからビルドしてください。

## ソースからビルド

Rust stable（1.95 以上。Slint 1.19 の要件）と、Windows では MSVC ビルドツールが必要です。

```powershell
git clone https://github.com/suraimu752/PULSE.git
cd PULSE
cargo build --release
.\target\release\PULSE.exe
```

デバッグビルドはコンソールを残します。リリースビルドはコンソールを出さず、起動失敗時はメッセージボックスを表示します。

## 必要な実行権限

管理者として実行しないでください。`PROCESS_QUERY_LIMITED_INFORMATION` で足りる情報だけを読みます。開けないプロセスは一覧には残し、CPU とメモリを `N/A` にします。

この版にプロセス終了機能はありません。

## 計測の制限事項

詳細は [docs/measurement.md](docs/measurement.md) です。要約:

- CPU は `GetSystemTimes` と `GetProcessTimes` の区間差分。100% は全論理プロセッサが埋まった状態
- 最初の約 0.5 秒は差分がないため CPU は `N/A`
- プロセスメモリはプライベートワーキングセット（`PROCESS_MEMORY_COUNTERS_EX2.PrivateWorkingSetSize`）。タスクマネージャーの「アクティブプライベートワーキングセット」とは一致しないことがある
- メモリは約 1 秒ごと、CPU は約 0.5 秒ごとに更新する。表示の再読込は約 0.25 秒で、計測とは別タイマー
- プロセス名は Toolhelp のイメージ名（`chrome.exe` など）。タスクマネージャーのフレンドリ名ではない
- PID 0 は Toolhelp では `[System Process]` のため、表示名だけ `System Idle Process` にする
- アイドルプロセスを開けない場合、その行は `N/A`。システム CPU は別計算なので、行が `N/A` でも全体 CPU は出る
- プロセスの I/O は `GetProcessIoCounters` で、ファイル以外も含む。物理ディスクの速度とは別
- プロセス別ネットワークは通常ユーザーでは取らない。画面は `要管理者`。アダプター速度は `GetIfTable2`
- GPU 使用率はエンジン種別の最大。専用と共有は別。カウンタが無いときは `なし`、読めないときは `N/A`
- 論理プロセッサ別ランキングは未実装
- Windows 上のディスク、ネットワーク、GPU の数値は実機では未検証。CI は API が落ちないことまで
- 詳細グラフは直近 60 秒。棒をクリックするとその時点の値と上位 5 を固定する。リングから落ちたあとも、選択中の 1 点だけは残る。60 秒より古い履歴をまとめて残すことはしない
- ディスクのグラフは物理ディスク、順位はプロセス I/O。プロセス別ネットワークは通常ユーザーでは順位を作らない
- 広告、テレメトリ、アカウント登録、常駐サービスはない

## ライセンス

[GPL-3.0-only](LICENSE)。依存関係の確認は [docs/licenses.md](docs/licenses.md)。

UI には Slint 1.19.0 を **GPL-3.0-only** として使っています。ロイヤリティフリーライセンスではないため、AboutSlint の表示義務はありません。

機能制限、広告、課金はありません。寄付先は未定のため `FUNDING.yml` は置いていません。

## バグ報告

[GitHub Issues](https://github.com/suraimu752/PULSE/issues) に、Windows の版、`PULSE.exe` を入手した Actions の実行 URL、再現手順、画面上の `N/A` と `0` の区別が分かる説明を書いてください。
