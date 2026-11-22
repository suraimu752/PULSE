# アーキテクチャ

Phase 1 のプロセスは二つです。

1. `pulse-sampler` スレッドが Collector を呼び、Aggregator で区間値にし、リングバッファへ押し、最新の `PublishedSample` を共有する
2. UI スレッド（Slint）が 250 ms ごとにそのスナップショットを読み、ソートして描画する

高頻度の ETW イベントを UI へ直接流す経路は作っていません。将来 ETW を入れる場合も、集計済みの区間値だけをスナップショットに載せます。

```
Windows API / PDH / GetIfTable2
        │
        ▼
 SystemProbe + MetricEnricher     （src/platform, src/collector.rs）
        │  RawSample（累積カウンタとゲージ）
        ▼
 Aggregator                        （src/aggregate.rs）
        │  Snapshot（使用率、B/s、全プロセス）
        ▼
 RingBuffer（60 秒、固定長）        （src/history.rs）
        │
        ▼
 PublishedSample（最新 + CPU/メモリの系列）
        │
        ▼
 present.rs → Slint               （UI は API を呼ばない）
```

## ディレクトリ

| パス | 内容 |
| --- | --- |
| `src/main.rs` | 起動と、失敗時のメッセージ |
| `src/lib.rs` | ライブラリ境界。テストから計算ロジックを呼ぶ |
| `src/config.rs` | 間隔と履歴長 |
| `src/metric.rs` | `Value` / `Unavailable` / `Unsupported` |
| `src/model.rs` | `RawSample` と `Snapshot` |
| `src/collector.rs` | 後から足す Collector のトレイト |
| `src/platform/windows.rs` | Toolhelp、GetProcessTimes、GetProcessMemoryInfo。CPU とメモリの式は変えない |
| `src/platform/disk.rs` | GetProcessIoCounters と PhysicalDisk PDH。物理転送量とは別 |
| `src/platform/network.rs` | GetIfTable2。プロセス帰属はしない |
| `src/platform/gpu.rs` | GPU Engine と GPU Process/Adapter Memory。使用率は最大 |
| `src/devices.rs` | デバイスごとの読み書きを、欠けを 0 にせず合わせる |
| `src/platform/stub.rs` | Windows 以外。数値を作らない |
| `src/aggregate.rs` | CPU% とバイト毎秒 |
| `src/history.rs` | リングバッファ |
| `src/sampler.rs` | スレッドと履歴の保持 |
| `src/sort.rs` | 表示時ソート |
| `src/format.rs` / `src/present.rs` | 日本語の表示文 |
| `ui/app.slint` | レイアウトと表示色。スクロールバー、折りたたみナビ、列幅、テーマはここだけで完結し、計測スレッドには渡さない |
| `src/ui.rs` | モデルの更新。`ui` feature のときだけコンパイル |

`ui` feature を外すと、Linux から `cargo check --target x86_64-pc-windows-gnu --no-default-features` で Windows 計測コードを型検査できます。Slint のクロスコンパイルは不要です。

## 拡張

ディスク、ネットワーク、GPU は `MetricEnricher` を `platform::make_enrichers` に追加する。

- プロセスのディスクとネットワークは累積バイト数。Aggregator が B/s にする
- GPU 使用率は Collector が区間ごとに正規化した値を入れる。エンジン使用率を単純加算しない
- システム全体のディスクとネットワークは、プロセス I/O の合計とは別フィールド（物理転送量と要求量を混ぜないため）

履歴に残るのは Aggregator 後の全プロセスです。詳細グラフの上位 5 は `src/rank.rs` が、選んだ 1 つのスナップショットだけから計算します。クリック位置からスナップショットへの対応は `src/pick.rs` です。選択中の 1 点はリングの外でも `Arc` で保持し、それ以外の過去は 60 秒で捨てます。

## スレッド

UI は `SamplerHandle` の `SharedLatest`（`Mutex` で包んだ `Arc<PublishedSample>`）だけを読む。ロックを持ったまま描画しない。ウィンドウが閉じると `SamplerHandle` の drop が停止フラグを立て、最大約 50 ms 以内にスレッドを join する。

プロセスハンドルはサンプルをまたいで保持しない。各周期で開き、読んで、閉じる。呼び出し回数の削減は後のフェーズで扱う。
