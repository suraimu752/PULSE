# ライセンス

確認日: 2026-11-20。対象は `Cargo.lock` を解決した依存です。

## このリポジトリ

`GPL-3.0-only`。`LICENSE` は GNU GPL 第 3 版の全文です。

Copyright (C) 2026 PULSE contributors

## 直接依存

| クレート | バージョン | クレートの SPDX | PULSE での選択 |
| --- | --- | --- | --- |
| slint | 1.19.0 | `GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0` | **GPL-3.0-only** |
| slint-build | 1.19.0（ビルド時のみ） | 上と同じ | **GPL-3.0-only** |
| windows | 0.62.2 | `MIT OR Apache-2.0` | どちらも GPL-3.0 と組み合わせられる |

Slint のロイヤリティフリーライセンス（デスクトップ向け、AboutSlint などの帰属が条件）と商用ライセンスは使いません。GPL-3.0-only を選ぶことで、PULSE 全体を GPL-3.0-only のまま配布できます。Slint 自身の説明でも、オープンソースのアプリは GPLv3 を使う、とされています。

`compat-1-18` を有効にしています。これは将来の Slint とのソース互換のための feature で、別ライセンスではありません。

有効にした Slint の feature: `std`、`backend-default`、`renderer-femtovg`、`renderer-software`、`accessibility`、`compat-1-18`。`system-tray` は無効です。

## 間接依存

`cargo metadata --format-version 1` で、このロックファイルから到達できるパッケージを確認しました（ホストが Linux のため、Windows 専用の任意依存は CI のロック解決と差が出ることがあります。直接依存のライセンスは同じです）。

到達したパッケージに、次のものはありませんでした。

- GPL-2.0-only（`or-later` ではないもの）
- AGPL
- 非商用条項（`CC-BY-NC` など）
- ライセンス欄が空のもの

GPL-3.0 と組み合わせるうえで注意した複数ライセンスは次のとおりです。いずれも、GPL と両立する選択肢を取れます。

| ライセンス表記 | 例 | 扱い |
| --- | --- | --- |
| Slint の三者ライセンス | `slint`、`i-slint-core`、`i-slint-renderer-femtovg`、`i-slint-renderer-software` など | GPL-3.0-only を選択 |
| `MIT OR Apache-2.0 OR LGPL-2.1-or-later` | `r-efi` | MIT または Apache-2.0 として受け取る。LGPL-2.1-or-later も GPL-3.0 とは両立するが、寛容ライセンス側を選ぶ |
| `BSL-1.0` | `clipboard-win`、`error-code`（Slint 経由） | Boost Software License は GPL-3.0 と両立する |
| `Unicode-3.0` | ICU のデータクレート | GPL-3.0 と両立する |
| MIT、Apache-2.0、BSD-2-Clause、BSD-3-Clause、Zlib、ISC、Unlicense、0BSD、CC0-1.0 | 大多数 | GPL-3.0 と両立する |

`windows` クレートは Windows ターゲットだけでリンクします。

## 再確認の手順

依存のバージョンを上げたら、次を実行してこの文書を更新します。

```bash
cargo metadata --format-version 1
```

GPL-2.0-only、AGPL、非商用、または条件が不明なライセンスが増えていたら、そのクレートは取り込まないでください。
