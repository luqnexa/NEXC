# Phase 2 — NEXC初期実装報告

この文書はPhase 2初期実装時点の記録です。最新の未確定入力・C ABIの追加は [Phase 3報告](phase3-report.md) を参照してください。

## 実装した内容

- NEXC-Core: 自前の接頭部分共有、品詞文字列テーブル、varint、CRC-32、境界検証。
- NEXC-Max: 正規化・重複排除・決定的なブロック構築、世代番号付き `.nud`、既存出力を上書きしない公開。
- NEXC-Lite: 索引からのディスク検索、最大0〜32ブロックのLRU、キャッシュ統計、全量検証・ストリーム復元。
- Lite追記: 1語ごとの軽量圧縮・同期、再起動後の検索、未完の最終レコード復旧、同時利用のlock。
- Base更新後の同一語重複を論理的に除き、ローカルの頻度指定を保持。
- `DictionarySource` を介した通常変換との統合。メモリ辞書と同じ候補生成・ランキングを利用。
- CLIの `dict build / verify / unpack / add`、`--nud / --journal`。
- 復元・破損・追記・キャッシュ・スレッド共有・CLIのテスト、容量・検索・追記ベンチマーク、CIへのスモーク追加。

既存圧縮ライブラリは使用していません。既知の基本技法を自前実装して独自形式に組み合わせた研究プロトタイプであり、圧縮原理の新規性や既存方式への優位性は未証明です。

## ファイル

新規作成:

- `crates/nexc-core/Cargo.toml`、`src/lib.rs`、`src/codec.rs`、`src/format.rs`。
- `crates/nexc-max/Cargo.toml`、`src/lib.rs`。
- `crates/nexc-lite/Cargo.toml`、`src/lib.rs`、`src/journal.rs`、`tests/storage.rs`。
- `crates/nextime-cli/src/commands.rs`。
- `benchmarks/nexc.rs`、`benchmarks/results/2026-10-11-nexc-windows.md`、同名 `.log`。
- `docs/nexc-format.md`、この報告。

変更:

- `Cargo.toml / Cargo.lock`: 3crateと依存関係を追加。
- `crates/nextime-dict/src/lib.rs`: 共通検索trait、Entry公開参照・検証、Arc対応。
- `crates/nextime-core/src/lib.rs`: 任意の辞書backendを所有。
- `crates/nextime-convert/src/lib.rs`: backendの検索とエラー伝播。
- `crates/nextime-cli/Cargo.toml / src/main.rs / tests/cli.rs`: NEXCコマンド・変換・結合テスト。
- `.github/workflows/ci.yml`: NEXCベンチマークのスモーク実行。
- `README.md / dictionaries/README.md / benchmarks/README.md / docs/design.md / docs/phase1-report.md`: 最新機能と仕様への参照、過去記録の明示。

Phase 1の機能と過去の実測記録を保持しています。削除した既存ファイルはありません。

## 検証

| 項目 | 結果 |
| --- | --- |
| `cargo fmt --all -- --check` | 成功 |
| `cargo clippy --workspace --all-targets -- -D warnings` | 成功、警告なし |
| `cargo test --workspace` | 成功、単体・結合34件 + doctest 1件 |
| `cargo build --workspace --release` | 成功 |
| `cargo +1.85.0 test --workspace --locked` | 成功、同じ35件 |
| `cargo +1.85.0 check --workspace --all-targets --locked` | 成功、ベンチマークを含めMSRVでコンパイル確認 |
| NEXC releaseベンチマーク、合成10万語 | 完走、全エントリ復元一致 |

Rust 1.98.1と最低対応版1.85.0 / Windowsで検証しています。GitHub ActionsはLinux・Windows・macOS用の設定を更新しましたが、GitHub上の実行や他OSでの確認はまだ行っていません。

## 実測

Ryzen 5 5600H、合成10万エントリ、候補8の単一実行結果:

- 正規化TSV: **4,210,000 bytes** → NUD: **983,649 bytes**（約23.36%）。
- 索引所有メモリ推計: **97,188 bytes**。プロセスRSSではありません。
- 変換p95: 8ブロックキャッシュ・反復 **0.0516ms**、分散 **0.4548ms**。
- キャッシュなしの分散変換p95: **0.7341ms**。
- 同じ分散入力のメモリ辞書p95: **0.0565ms**。NEXCの分散検索は遅くなっています。
- 圧縮追記・`sync_data` のp95: **1.8034ms**。通常変換とは別計測です。

条件・全分位点・キャッシュ統計・限界は [実測記録](../benchmarks/results/2026-10-11-nexc-windows.md)、形式は [仕様書](nexc-format.md) を参照してください。

## 未達成・未実装

この実装はPhase 2の基盤です。1億エントリ、容量1GiB/512MiB、iPhone SEのp95 5ms、電力・熱・低電力時の目標は未検証です。Maxの全量RAMソートは巨大辞書に適さず、索引も全量RAMに保持します。部分読みごとのブロック検索・復元が遅延要因です。

語幹・接辞の最適化、言語ID・言語別索引、Knowledge-Aware Compressionの学習、差分生成、世代管理の運用、追記ジャーナルの物理的な整理、削除、暗号化、SHA-256/Ed25519、署名Manifestと原子的な現行辞書切り替えは未実装です。CRCは署名の代わりになりません。

追記は明示登録だけを端末内に保存します。通常変換入力を保存・送信しません。ジャーナルは暗号化していないため、保護済みのPersonal Dictionaryはまだ完成していません。クラッシュ後のstale lockは利用者による確認と除去が必要です。

## 次に必要な作業

まずライセンス確認済みの代表的な実語彙を用意し、既存圧縮器・検索構造との同条件比較を行います。外部ソート・ストリーミング構築と索引圧縮で規模を拡大し、部分読み検索を改善します。1億語とiPhone SE実機の測定を用意し、容量・RSS・変換遅延を同時に評価します。その後、Phase 3のFFI・preedit・OSアダプターへ接続します。
