# Bundled fonts

## `SymbolsNerdFontMono-Regular.ttf`

The Nerd Fonts symbols face (family `Symbols Nerd Font Mono`), which draws Nerd Font icons
(Private Use Area codepoints) in surfaces' icon spans and in the grid.
`auk_text::NERD_SYMBOLS` embeds it.

- Source: `NerdFontsSymbolsOnly.zip` from the Nerd Fonts v3.4.0 release,
  <https://github.com/ryanoasis/nerd-fonts/releases/download/v3.4.0/NerdFontsSymbolsOnly.zip>
  (zip sha256 `8e617904b980fe3648a4b116808788fe50c99d2d495376cb7c0badbd8a564c47`). The same
  release `crates/views/src/icon_table.rs` was generated from.
- File sha256: `f0f624d9b474bea1662cf7e862d44aebe1ae1f6c7f9cb7a0ca5d0e5ac9561c60`. Name table:
  "Symbols Nerd Font Mono", "Version 001.000;Nerd Fonts 3.4.0", "Copyright (c) 2016, Ryan
  McIntyre".
- License: `LICENSE-NerdFonts.md` is the Nerd Fonts repository's `LICENSE` at v3.4.0: patched
  and glyph fonts are under the SIL Open Font License 1.1, the project's scripts under MIT (the
  release zip carries only the MIT text). The OFL allows bundling and embedding the font in
  software, provided it isn't sold by itself and a modified version doesn't use the reserved
  name; Auk ships it unmodified.
- The glyphs come from icon sets with their own licenses (Nerd Fonts' `license-audit.md` at
  v3.4.0): Codicons and Font Awesome (CC BY 4.0), Material Design Icons (Apache 2.0), Devicons,
  Font Awesome Extension, IEC Power Symbols, Seti-UI, Octicons and Powerline Extra Symbols
  (MIT), Pomicons and Weather Icons (OFL 1.1), Powerline Symbols (free license), Font Logos
  (unlicensed). CC BY 4.0 asks for attribution, which this file and `LICENSE-NerdFonts.md`
  give.
