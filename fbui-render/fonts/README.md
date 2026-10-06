# Bundled fonts

- `Inter-Regular.ttf` — Inter, by The Inter Project Authors, under the SIL Open
  Font License 1.1 (`Inter-LICENSE.txt`). Compiled in by the `bundled-font`
  feature.
- `Inter-12.fbf` … `Inter-24.fbf` — bitmap fonts rasterized from that file
  (Latin-1 plus common punctuation, 4-bit coverage) by
  `cargo run -p fbui-render --example make_bitmap_font -- fonts/Inter-Regular.ttf fonts/Inter 12,16,20,24`.
  They are derived from Inter and distributed under the same license. Compiled
  in by the `bundled-bitmap-font` feature; see `src/text/bitmap.rs` for the
  format.
