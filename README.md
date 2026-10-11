# NEXC

Luqnexaの独自辞書圧縮エンジンです。Rustの `nexc-core`（バイナリ形式・独自符号化）、`nexc-max`（辞書構築）、`nexc-lite`（ディスク検索・追記辞書）と共通の `nextime-dict` を収録します。既存の圧縮ライブラリを内部エンジンとして使用しません。

研究用プロトタイプです。1億エントリ・1GiB以下・iPhone SEでのp95 5ms達成は未検証です。署名更新・クラウド配信は未実装です。

```sh
cargo test --workspace --locked
cargo build --workspace --release --locked
cargo bench -p nexc-lite --bench nexc -- --entries 1000 --iterations 100
```

ベンチマークは合成データの圧縮・検索・追記を実測します。IME全体の変換比較と元の統合テストは [Luqnexa](https://github.com/Luqnexa/Luqnexa) に収録しています。

ソースはAGPL-3.0-only。手書きの小規模辞書は再現用フィクスチャです。形式は [docs/nexc-format.md](docs/nexc-format.md) を参照してください。
