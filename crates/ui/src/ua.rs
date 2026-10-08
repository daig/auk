//! The user-agent sheet: a subset of WebKit's, covering the tags `el` allows (§2.9) plus the
//! document root and body.

pub const UA_CSS: &str = r#"
html, body, div, p, section, header, footer, nav, aside, main, article, figure, figcaption, blockquote,
ul, ol, dl, dt, dd, h1, h2, h3, h4, h5, h6, pre, form, hr, address, details, summary, fieldset, legend {
  display: block;
}
head, script, style, title, template, meta, link { display: none; }
html { font-family: system-ui; font-size: 16px; color: black; }
body { margin: 8px; }
p { margin-top: 1em; margin-bottom: 1em; }
blockquote, figure { margin: 1em 40px; }
h1 { font-size: 2em; margin-top: 0.67em; margin-bottom: 0.67em; font-weight: bold; }
h2 { font-size: 1.5em; margin-top: 0.83em; margin-bottom: 0.83em; font-weight: bold; }
h3 { font-size: 1.17em; margin-top: 1em; margin-bottom: 1em; font-weight: bold; }
h4 { margin-top: 1.33em; margin-bottom: 1.33em; font-weight: bold; }
h5 { font-size: 0.83em; margin-top: 1.67em; margin-bottom: 1.67em; font-weight: bold; }
h6 { font-size: 0.67em; margin-top: 2.33em; margin-bottom: 2.33em; font-weight: bold; }
ul, ol { margin-top: 1em; margin-bottom: 1em; padding-left: 40px; }
ul { list-style-type: disc; }
ol { list-style-type: decimal; }
ul ul, ol ul { list-style-type: circle; }
ul ul, ul ol, ol ul, ol ol { margin-top: 0; margin-bottom: 0; }
li { display: list-item; }
dl { margin-top: 1em; margin-bottom: 1em; }
dd { margin-left: 40px; }
pre, code, kbd, samp, tt { font-family: monospace; }
pre { white-space: pre; margin-top: 1em; margin-bottom: 1em; }
hr { margin: 0.5em auto; border-style: inset; border-width: 1px; color: gray; overflow: hidden; }
strong, b { font-weight: bolder; }
em, i { font-style: italic; }
del { text-decoration: line-through; }
mark { background-color: yellow; color: black; }
table { display: table; border-collapse: separate; border-spacing: 2px; border-color: gray; box-sizing: border-box; }
thead { display: table-header-group; vertical-align: middle; }
tbody { display: table-row-group; vertical-align: middle; }
tfoot { display: table-footer-group; vertical-align: middle; }
tr { display: table-row; vertical-align: inherit; }
td, th { display: table-cell; vertical-align: inherit; padding: 1px; }
th { font-weight: bold; text-align: center; }
col { display: table-column; }
colgroup { display: table-column-group; }
button, input { display: inline-block; }
input[type=checkbox], input[type=radio] { width: 12px; height: 12px; margin: 3px 2px; box-sizing: border-box; }
"#;
