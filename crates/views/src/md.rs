//! Markdown in omp's dialect (§8.5, research/elements.md §7.1): CommonMark plus GFM tables, task
//! lists, strikethrough, autolinks and callouts; math delimiters; raw HTML shown literally.
//! Parsed per top-level block: an update re-renders only the blocks whose source changed, and a
//! streaming append re-parses from the last block.

use crate::dom::Dom;
use crate::highlight;
use crate::spans::{self, Span};
use auk_ui::NodeId;
use pulldown_cmark::{
    Alignment, BlockQuoteKind, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd,
};
use std::ops::Range;

fn options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_GFM
        | Options::ENABLE_MATH
        | Options::ENABLE_FOOTNOTES
}

/// One top-level block.
#[derive(Debug)]
pub(crate) struct Block {
    pub src: String,
    pub el: NodeId,
}

/// A `md` node's render state.
#[derive(Debug, Default)]
pub(crate) struct MdState {
    pub source: String,
    /// Where each top-level block starts and ends in `source`.
    pub ranges: Vec<Range<usize>>,
    pub blocks: Vec<Block>,
    /// The reference definitions' source lines (a change re-renders everything).
    pub refdefs: String,
    pub caret: Option<NodeId>,
    pub stream: bool,
    pub marks: Vec<Span>,
    /// Blocks re-rendered by the last update (for checking incremental work).
    pub rerendered: usize,
    /// Device pixels per em of math (set by the kind before an update; 0 picks 13px at 1x).
    pub math_em: u32,
    /// Formulas typesetting in the background, filled in by the clock.
    pub math_pending: Vec<crate::math::Pending>,
    /// The theme and density mermaid figures were drawn for.
    pub env: MdEnv,
    /// Mermaid figures whose diagram is being drawn in the background.
    pub figs: Vec<Fig>,
}

/// What mermaid figures are drawn for: the theme and the density (image px per CSS px).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct MdEnv {
    pub dark: bool,
    pub density: f32,
    /// The host has an image viewer: figures draw Open.
    pub viewer: bool,
}

/// A mermaid figure waiting for its picture.
#[derive(Debug)]
pub(crate) struct Fig {
    pub fig: NodeId,
    pub body: NodeId,
    pub id: String,
    pub kind: &'static str,
    pub src: String,
}

/// Top-level block ranges of `text`, and the reference definition lines.
fn split(text: &str) -> (Vec<Range<usize>>, String) {
    let parser = Parser::new_ext(text, options());
    let mut refdefs = String::new();
    for (_, def) in parser.reference_definitions().iter() {
        if let Some(src) = text.get(def.span.clone()) {
            refdefs.push_str(src.trim_end());
            refdefs.push('\n');
        }
    }
    let mut depth = 0usize;
    let mut out: Vec<Range<usize>> = Vec::new();
    for (ev, r) in Parser::new_ext(text, options()).into_offset_iter() {
        match ev {
            Event::Start(_) => {
                if depth == 0 {
                    out.push(r);
                }
                depth += 1;
            }
            Event::End(_) => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(r),
            _ => {}
        }
    }
    (out, refdefs)
}

impl MdState {
    /// Bring the blocks up to `text` under `holder`. Returns whether anything changed.
    pub fn update(
        &mut self,
        dom: &mut Dom,
        holder: NodeId,
        text: &str,
        stream: bool,
        marks: Vec<Span>,
        env: MdEnv,
    ) -> bool {
        if text == self.source && stream == self.stream && marks == self.marks && env == self.env {
            self.rerendered = 0;
            return false;
        }
        // A new theme or density draws the diagrams again; the end of a stream may close a
        // mermaid fence that showed as code.
        let env_changed = env != self.env;
        let stream_ended = self.stream && !stream;
        self.env = env;
        let marks_changed = marks != self.marks;
        // Re-split from the last block when the text only grew.
        let (ranges, refdefs) = if !marks_changed
            && text.starts_with(&self.source)
            && let Some(last) = self.ranges.last()
        {
            let from = last.start;
            let (tail, tail_defs) = split(&text[from..]);
            let mut ranges: Vec<Range<usize>> = self.ranges[..self.ranges.len() - 1].to_vec();
            ranges.extend(tail.into_iter().map(|r| r.start + from..r.end + from));
            let defs = if tail_defs.is_empty() || self.refdefs.contains(&tail_defs) {
                self.refdefs.clone()
            } else {
                // New definitions: the full set.
                split(text).1
            };
            (ranges, defs)
        } else {
            split(text)
        };
        let all = marks_changed || refdefs != self.refdefs;
        self.refdefs = refdefs;
        self.marks = marks;
        if let Some(c) = self.caret.take()
            && dom.doc.try_node(c).is_some()
        {
            dom.doc.remove(c);
        }
        let mut rerendered = 0;
        let mut blocks: Vec<Block> = Vec::with_capacity(ranges.len());
        let mut old = std::mem::take(&mut self.blocks).into_iter();
        let old_last = self.ranges.len().checked_sub(1);
        let n = ranges.len();
        for (i, r) in ranges.iter().enumerate() {
            let src = &text[r.clone()];
            let prev = old.next();
            // Provisional: the streamed tail, where an open mermaid fence stays code.
            let provisional = stream && i + 1 == n;
            let mermaid = has_mermaid(src);
            let redo_fig = mermaid && (env_changed || (stream_ended && Some(i) == old_last));
            match prev {
                Some(b) if !all && !redo_fig && b.src == src => {
                    blocks.push(b);
                }
                Some(b) => {
                    let el = self.render_block(dom, src, provisional);
                    dom.doc.insert_before(holder, el, Some(b.el));
                    dom.doc.remove(b.el);
                    blocks.push(Block {
                        src: src.to_owned(),
                        el,
                    });
                    rerendered += 1;
                }
                None => {
                    let el = self.render_block(dom, src, provisional);
                    dom.doc.append_child(holder, el);
                    blocks.push(Block {
                        src: src.to_owned(),
                        el,
                    });
                    rerendered += 1;
                }
            }
            let _ = i;
        }
        for b in old {
            if dom.doc.try_node(b.el).is_some() {
                dom.doc.remove(b.el);
            }
        }
        // Figures of blocks drawn again are gone.
        self.figs.retain(|f| {
            dom.doc.try_node(f.fig).is_some() && blocks.iter().any(|b| is_within(dom, f.fig, b.el))
        });
        let n = blocks.len();
        for (i, b) in blocks.iter().enumerate() {
            dom.set_class(
                b.el,
                if stream && i + 1 == n {
                    "md streaming"
                } else {
                    "md"
                },
            );
        }
        if stream {
            let target = blocks
                .last()
                .map(|b| last_inline_container(dom, b.el))
                .unwrap_or(holder);
            let c = dom.child(target, "span", "md-caret");
            dom.attr(c, "aria-hidden", "true");
            self.caret = Some(c);
        }
        self.blocks = blocks;
        self.ranges = ranges;
        self.source = text.to_owned();
        self.stream = stream;
        self.rerendered = rerendered;
        true
    }

    fn render_block(&mut self, dom: &mut Dom, src: &str, provisional: bool) -> NodeId {
        let el = dom.el("div", "md");
        let src = math_delimiters(src);
        let full;
        let input = if self.refdefs.is_empty() {
            &*src
        } else {
            full = format!("{src}\n\n{}", self.refdefs);
            &full
        };
        let em = if self.math_em == 0 {
            crate::math::em_px(13.0, 1.0, 1.0)
        } else {
            self.math_em
        };
        let fig = FigCx {
            env: self.env,
            // A fence left open while streaming stays code (book:5000).
            open_fence: provisional && !fences_closed(&src),
            figs: &mut self.figs,
        };
        render_events(
            dom,
            el,
            Parser::new_ext(input, options()),
            &self.marks,
            (em, &mut self.math_pending),
            fig,
        );
        el
    }

    /// Draws in the diagrams finished in the background, and turns the ones that failed into
    /// code. Returns whether any is still being drawn.
    pub fn poll_figures(&mut self, dom: &mut Dom) -> bool {
        if self.figs.is_empty() {
            return false;
        }
        crate::image::pump();
        let density = self.env.density;
        let mut waiting = Vec::new();
        for f in std::mem::take(&mut self.figs) {
            if dom.doc.try_node(f.fig).is_none() {
                continue;
            }
            match crate::mermaid::peek(&f.id, density) {
                None => waiting.push(f),
                Some(Ok(pic)) => {
                    dom.clear(f.body);
                    picture_into(dom, f.body, f.kind, &pic);
                }
                Some(Err(e)) => {
                    let parent = dom.doc.parent(f.fig);
                    if let Some(parent) = parent {
                        let holder = dom.el("div", "code");
                        dom.doc.insert_before(parent, holder, Some(f.fig));
                        dom.doc.remove(f.fig);
                        fallback(dom, holder, &f.src, &e);
                    }
                }
            }
        }
        self.figs = waiting;
        !self.figs.is_empty()
    }
}

/// Whether `el` is `root` or inside it.
fn is_within(dom: &Dom, mut el: NodeId, root: NodeId) -> bool {
    loop {
        if el == root {
            return true;
        }
        match dom.doc.parent(el) {
            Some(p) => el = p,
            None => return false,
        }
    }
}

/// Whether a block's source has a mermaid fence (its figure depends on the theme).
fn has_mermaid(src: &str) -> bool {
    src.lines().any(|l| {
        let t = l.trim_start_matches([' ', '>', '-', '*', '+']).trim_start();
        let rest = t
            .strip_prefix("```")
            .or_else(|| t.strip_prefix("~~~"))
            .map(|r| r.trim_start_matches(['`', '~']).trim_start());
        rest.is_some_and(|r| r.starts_with("mermaid"))
    })
}

/// Whether every code fence in a block's source is closed.
fn fences_closed(src: &str) -> bool {
    let mut open: Option<(char, usize)> = None;
    for l in src.lines() {
        let t = l.trim_start_matches([' ', '>', '-', '*', '+']).trim_start();
        let Some(c) = t.chars().next().filter(|c| *c == '`' || *c == '~') else {
            continue;
        };
        let n = t.chars().take_while(|x| *x == c).count();
        if n < 3 {
            continue;
        }
        match open {
            None => open = Some((c, n)),
            Some((oc, on)) if oc == c && n >= on && t[n..].trim().is_empty() => open = None,
            Some(_) => {}
        }
    }
    open.is_none()
}

/// What a block's mermaid fences need.
struct FigCx<'f> {
    env: MdEnv,
    open_fence: bool,
    figs: &'f mut Vec<Fig>,
}

/// A mermaid fence: `figure.mfig[data-m] > div.mfig-head (icon, "mermaid · <kind>", Open) +
/// div.mfig-body` with the diagram at natural size (book:5018). While the diagram is drawn in
/// the background the body shows its source; one that can't be drawn is a code block with a note.
fn mermaid_block(dom: &mut Dom, parent: NodeId, text: &str, fig: &mut FigCx) {
    let text = text.strip_suffix('\n').unwrap_or(text);
    let kind = match crate::mermaid::kind(text) {
        Ok(k) => k,
        Err(k) => {
            let holder = dom.child(parent, "div", "code");
            fallback(dom, holder, text, &crate::mermaid::unsupported(&k));
            return;
        }
    };
    let (id, poll) = crate::mermaid::request(text, fig.env.dark, fig.env.density);
    if let crate::image::Poll::Ready(Err(e)) = &poll {
        let holder = dom.child(parent, "div", "code");
        fallback(dom, holder, text, e);
        return;
    }
    let f = dom.child(parent, "figure", "mfig");
    dom.attr(f, "data-m", text);
    let head = dom.child(f, "div", "mfig-head");
    let ic = dom.child(head, "span", "mfig-ic");
    if let Some(g) = crate::icons::glyph("diagram") {
        dom.text(ic, &g.to_string());
    }
    dom.child_text(head, "span", "mfig-kind", &format!("mermaid · {kind}"));
    if fig.env.viewer {
        let open = dom.child_text(head, "button", "btn sm quiet mfig-open", "Open");
        dom.attr(open, "data-mfig", &id);
        dom.attr(open, "title", "Show the diagram large");
    }
    let body = dom.child(f, "div", "mfig-body");
    match poll {
        crate::image::Poll::Ready(Ok(pic)) => picture_into(dom, body, kind, &pic),
        _ => {
            dom.attr(f, "aria-busy", "true");
            let pre = dom.child(body, "pre", "mfig-src");
            dom.child_text(pre, "code", "", text);
            fig.figs.push(Fig {
                fig: f,
                body,
                id,
                kind,
                src: text.to_owned(),
            });
        }
    }
}

/// The diagram's picture in a figure's body, at its natural size.
fn picture_into(dom: &mut Dom, body: NodeId, kind: &str, pic: &crate::image::Picture) {
    if let Some(f) = dom.doc.parent(body) {
        dom.doc.remove_attribute(f, "aria-busy");
    }
    let el = dom.child(body, "svg", "mfig-svg");
    dom.attr(el, "role", "img");
    dom.attr(el, "aria-label", &format!("{kind} diagram"));
    let mut content = pic.frames[0].content.clone();
    content.natural = pic.size;
    dom.set_style(
        el,
        &format!(
            "width: {}px; height: {}px",
            crate::common::fmt_num(f64::from(pic.size.0)),
            crate::common::fmt_num(f64::from(pic.size.1))
        ),
    );
    dom.doc.set_image(el, Some(content));
}

/// A mermaid fence that isn't drawn: the code block, and a quiet note saying why.
fn fallback(dom: &mut Dom, holder: NodeId, text: &str, why: &str) {
    code_block(dom, holder, Some("mermaid"), text);
    let note = dom.child_text(holder, "div", "mfig-note", &format!("Not drawn: {why}"));
    dom.attr(note, "title", why);
}

/// The deepest last element that holds inline content, where the streaming caret goes.
fn last_inline_container(dom: &Dom, mut el: NodeId) -> NodeId {
    loop {
        let last = dom
            .doc
            .children(el)
            .iter()
            .rev()
            .copied()
            .find(|c| dom.doc.node(*c).is_element());
        let Some(last) = last else { return el };
        let tag = dom
            .doc
            .node(last)
            .element()
            .map(|e| e.tag.as_str().to_owned())
            .unwrap_or_default();
        let class = dom.class_of(last).to_owned();
        match tag.as_str() {
            "ul" | "ol" | "li" | "blockquote" | "table" | "thead" | "tbody" | "tr" => el = last,
            "div"
                if class.starts_with("callout") || class == "co-body" || class == "md-li-body" =>
            {
                el = last
            }
            "p" | "h1" | "h2" | "h3" | "h4" | "td" | "th" => return last,
            _ => return el,
        }
    }
}

struct Renderer<'d, 'a, 'm, 'f> {
    dom: &'d mut Dom<'a>,
    stack: Vec<NodeId>,
    marks: &'m [Span],
    fig: FigCx<'f>,
    /// Inside a code block: (parent, lang, text).
    code: Option<(NodeId, Option<String>, String)>,
    html: Option<(NodeId, String)>,
    /// Table state: alignments, in head, column index.
    table: Option<(Vec<Alignment>, bool, usize)>,
    /// Ordered-list counters.
    lists: Vec<Option<u64>>,
    in_link: usize,
    /// Math's device pixels per em, and the formulas left to the background.
    math: (u32, &'m mut Vec<crate::math::Pending>),
    /// Per paragraph, in order: whether it holds nothing but one display formula.
    math_alone: Vec<bool>,
    /// The paragraph being drawn (an index into `math_alone`), if inside one.
    para: usize,
    in_para: bool,
}

fn render_events<'e>(
    dom: &mut Dom,
    holder: NodeId,
    events: impl Iterator<Item = Event<'e>>,
    marks: &[Span],
    math: (u32, &mut Vec<crate::math::Pending>),
    fig: FigCx,
) {
    let events: Vec<Event<'e>> = events.collect();
    // Which paragraphs are one display formula and nothing else (whitespace and line breaks
    // aside).
    let mut math_alone = Vec::new();
    let mut cur: Option<(usize, bool)> = None;
    for ev in &events {
        match ev {
            Event::Start(Tag::Paragraph) => cur = Some((0, true)),
            Event::End(TagEnd::Paragraph) => {
                if let Some((n, only)) = cur.take() {
                    math_alone.push(only && n == 1);
                }
            }
            Event::DisplayMath(_) => {
                if let Some((n, _)) = &mut cur {
                    *n += 1;
                }
            }
            Event::SoftBreak | Event::HardBreak => {}
            Event::Text(t) if t.trim().is_empty() => {}
            _ => {
                if let Some((_, only)) = &mut cur {
                    *only = false;
                }
            }
        }
    }
    let mut r = Renderer {
        dom,
        stack: vec![holder],
        marks,
        fig,
        code: None,
        html: None,
        table: None,
        lists: Vec::new(),
        in_link: 0,
        math,
        math_alone,
        para: 0,
        in_para: false,
    };
    for ev in events {
        let end_para = matches!(ev, Event::End(TagEnd::Paragraph));
        if matches!(ev, Event::Start(Tag::Paragraph)) {
            r.in_para = true;
        }
        r.event(ev);
        if end_para {
            r.para += 1;
            r.in_para = false;
        }
    }
}

impl Renderer<'_, '_, '_, '_> {
    fn top(&self) -> NodeId {
        *self.stack.last().expect("holder")
    }

    /// A typeset formula (book: md Math).
    fn math(&mut self, tex: &str, display: bool) {
        let p = self.top();
        // Display math alone in its paragraph is a block (`div.math-display`); amid the
        // paragraph's text it is `span.math.display`, inline-level in the `p`.
        let block =
            display && (!self.in_para || self.math_alone.get(self.para).copied().unwrap_or(false));
        let (_, pending) = crate::math::mount(self.dom, p, tex, display, block, self.math.0);
        self.math.1.extend(pending);
    }

    fn open(&mut self, tag: &str, class: &str) -> NodeId {
        let p = self.top();
        let e = self.dom.child(p, tag, class);
        self.stack.push(e);
        e
    }

    fn close(&mut self) {
        if self.stack.len() > 1 {
            self.stack.pop();
        }
    }

    fn event(&mut self, ev: Event) {
        if let Some((_, _, text)) = &mut self.code {
            match ev {
                Event::Text(t) => {
                    text.push_str(&t);
                    return;
                }
                Event::End(TagEnd::CodeBlock) => {
                    let (parent, lang, text) = self.code.take().unwrap();
                    if lang.as_deref() == Some("mermaid") && !self.fig.open_fence {
                        mermaid_block(self.dom, parent, &text, &mut self.fig);
                    } else {
                        let holder = self.dom.child(parent, "div", "code");
                        code_block(self.dom, holder, lang.as_deref(), &text);
                    }
                    return;
                }
                _ => return,
            }
        }
        if let Some((_, text)) = &mut self.html {
            match ev {
                Event::Html(t) | Event::Text(t) => {
                    text.push_str(&t);
                    return;
                }
                Event::End(TagEnd::HtmlBlock) => {
                    let (holder, text) = self.html.take().unwrap();
                    let t = text.strip_suffix('\n').unwrap_or(&text);
                    self.dom.text(holder, t);
                    return;
                }
                _ => return,
            }
        }
        match ev {
            Event::Start(tag) => self.start(tag),
            Event::End(end) => self.end(end),
            Event::Text(t) => self.prose(&t),
            Event::Code(t) => {
                let p = self.top();
                let c = self.dom.child(p, "code", "");
                if is_hex_color(&t) {
                    let sw = self.dom.child(c, "span", "md-swatch");
                    self.dom.set_style(sw, &format!("background-color: {t}"));
                }
                self.dom.text(c, &t);
            }
            Event::InlineMath(t) => self.math(&t, false),
            Event::DisplayMath(t) => self.math(&t, true),
            Event::Html(t) | Event::InlineHtml(t) => {
                let p = self.top();
                self.dom.text(p, &t);
            }
            Event::FootnoteReference(l) => {
                let p = self.top();
                self.dom.child_text(p, "span", "md-fn", &format!("[{l}]"));
            }
            Event::SoftBreak => {
                let p = self.top();
                self.dom.text(p, " ");
            }
            Event::HardBreak => {
                let p = self.top();
                self.dom.child_text(p, "span", "md-br", "\n");
            }
            Event::Rule => {
                let p = self.top();
                self.dom.child(p, "hr", "");
            }
            Event::TaskListMarker(done) => {
                let p = self.top();
                let t = self
                    .dom
                    .child(p, "span", if done { "md-task on" } else { "md-task" });
                self.dom.attr(t, "role", "checkbox");
                self.dom
                    .attr(t, "aria-checked", if done { "true" } else { "false" });
                if done && let Some(g) = crate::icons::glyph("check") {
                    self.dom.text(t, &g.to_string());
                }
                // The marker sits in the item, not in its bullet.
                if let Some(li) = self.dom.doc.parent(p)
                    && self.dom.class_of(p) == "md-li-body"
                {
                    let kids = self.dom.doc.children(li).to_vec();
                    if let Some(&mark) = kids.first()
                        && self.dom.class_of(mark) == "md-li-mark"
                    {
                        self.dom.doc.remove(mark);
                        self.dom.set_class(li, "task");
                    }
                }
            }
        }
    }

    fn start(&mut self, tag: Tag) {
        match tag {
            Tag::Paragraph => {
                self.open("p", "");
            }
            Tag::Heading { level, .. } => {
                let (t, c) = match level {
                    HeadingLevel::H1 => ("h1", ""),
                    HeadingLevel::H2 => ("h2", ""),
                    HeadingLevel::H3 => ("h3", ""),
                    HeadingLevel::H4 => ("h4", ""),
                    HeadingLevel::H5 => ("h4", "h5"),
                    HeadingLevel::H6 => ("h4", "h6"),
                };
                self.open(t, c);
            }
            Tag::BlockQuote(kind) => match kind {
                Some(k) => {
                    let name = match k {
                        BlockQuoteKind::Note => "note",
                        BlockQuoteKind::Tip => "tip",
                        BlockQuoteKind::Important => "important",
                        BlockQuoteKind::Warning => "warning",
                        BlockQuoteKind::Caution => "caution",
                    };
                    let co = self.open("div", &format!("callout {name}"));
                    let ic = self.dom.child(co, "span", "co-ic");
                    let icon = match name {
                        "note" => "info",
                        "tip" => "lightbulb",
                        "important" => "bell",
                        "warning" => "warn",
                        _ => "shield-alert",
                    };
                    if let Some(g) = crate::icons::glyph(icon) {
                        self.dom.text(ic, &g.to_string());
                    }
                    let body = self.dom.child(co, "div", "co-body");
                    self.dom
                        .child_text(body, "div", "co-title", &title_case(name));
                    self.stack.push(body);
                }
                None => {
                    self.open("blockquote", "");
                }
            },
            Tag::CodeBlock(kind) => {
                let lang = match kind {
                    CodeBlockKind::Fenced(l) => {
                        let l = l.split_whitespace().next().unwrap_or("").to_owned();
                        (!l.is_empty()).then_some(l)
                    }
                    CodeBlockKind::Indented => None,
                };
                let p = self.top();
                self.code = Some((p, lang, String::new()));
            }
            Tag::HtmlBlock => {
                let p = self.top();
                let h = self.dom.child(p, "div", "md-html");
                self.html = Some((h, String::new()));
            }
            Tag::List(start) => {
                match start {
                    Some(n) => {
                        let ol = self.open("ol", "");
                        if n != 1 {
                            self.dom.attr(ol, "start", &n.to_string());
                        }
                    }
                    None => {
                        self.open("ul", "");
                    }
                }
                self.lists.push(start);
            }
            Tag::Item => {
                let li = self.open("li", "");
                let mark = match self.lists.last_mut() {
                    Some(Some(n)) => {
                        let s = format!("{n}.");
                        *n += 1;
                        s
                    }
                    _ => "•".to_owned(),
                };
                self.dom.child_text(li, "span", "md-li-mark", &mark);
                let body = self.dom.child(li, "div", "md-li-body");
                self.stack.push(body);
            }
            Tag::FootnoteDefinition(l) => {
                let d = self.open("div", "md-fndef");
                self.dom.child_text(d, "span", "md-fn", &format!("[{l}] "));
            }
            Tag::DefinitionList => {
                self.open("dl", "");
            }
            Tag::DefinitionListTitle => {
                self.open("dt", "");
            }
            Tag::DefinitionListDefinition => {
                self.open("dd", "");
            }
            Tag::Table(aligns) => {
                let wrap = self.open("div", "md-table");
                let _ = wrap;
                self.open("table", "");
                self.table = Some((aligns, false, 0));
            }
            Tag::TableHead => {
                self.open("thead", "");
                self.open("tr", "");
                if let Some(t) = &mut self.table {
                    t.1 = true;
                    t.2 = 0;
                }
            }
            Tag::TableRow => {
                // Body rows go in a tbody.
                let top = self.top();
                let tag = self.tag_of(top);
                if tag == "table" {
                    self.open("tbody", "");
                }
                self.open("tr", "");
                if let Some(t) = &mut self.table {
                    t.2 = 0;
                }
            }
            Tag::TableCell => {
                let (head, align) = match &self.table {
                    Some((a, h, i)) => (*h, a.get(*i).copied().unwrap_or(Alignment::None)),
                    None => (false, Alignment::None),
                };
                let class = match align {
                    Alignment::Left => "ta-start",
                    Alignment::Center => "ta-center",
                    Alignment::Right => "ta-end",
                    Alignment::None => "",
                };
                self.open(if head { "th" } else { "td" }, class);
                if let Some(t) = &mut self.table {
                    t.2 += 1;
                }
            }
            Tag::Emphasis => {
                self.open("em", "");
            }
            Tag::Strong => {
                self.open("strong", "");
            }
            Tag::Strikethrough => {
                self.open("del", "");
            }
            Tag::Superscript | Tag::Subscript => {
                self.open("span", "md-sup");
            }
            Tag::Link {
                dest_url, title, ..
            } => {
                let l = self.open("span", "sf-link");
                self.dom.attr(l, "data-href", &dest_url);
                let tip = if title.is_empty() {
                    format!("{dest_url}  ⌘-click to open")
                } else {
                    format!("{title}\n{dest_url}")
                };
                self.dom.attr(l, "title", &tip);
                self.in_link += 1;
            }
            Tag::Image {
                dest_url, title, ..
            } => {
                let i = self.open("span", "md-img");
                self.dom.attr(i, "data-src", &dest_url);
                if !title.is_empty() {
                    self.dom.attr(i, "title", &title);
                }
            }
            Tag::MetadataBlock(_) => {
                self.open("div", "md-meta");
            }
        }
    }

    fn tag_of(&self, id: NodeId) -> String {
        self.dom
            .doc
            .node(id)
            .element()
            .map(|e| e.tag.as_str().to_owned())
            .unwrap_or_default()
    }

    fn end(&mut self, end: TagEnd) {
        match end {
            TagEnd::Item => {
                // body, li
                self.close();
                self.close();
            }
            TagEnd::List(_) => {
                self.lists.pop();
                self.close();
            }
            TagEnd::TableHead => {
                self.close();
                self.close();
                if let Some(t) = &mut self.table {
                    t.1 = false;
                }
            }
            TagEnd::TableCell => {
                let cell = self.top();
                // Severity pills: a cell that is exactly high, medium or low.
                let kids = self.dom.doc.children(cell).to_vec();
                if kids.len() == 1
                    && let Some(t) = self.dom.doc.node(kids[0]).text().map(str::to_owned)
                    && matches!(
                        t.trim().to_ascii_lowercase().as_str(),
                        "high" | "medium" | "low"
                    )
                {
                    self.dom.doc.remove(kids[0]);
                    let pill = self.dom.child(cell, "span", "md-sev");
                    self.dom
                        .attr(pill, "data-sev", &t.trim().to_ascii_lowercase());
                    self.dom.text(pill, t.trim());
                }
                self.close();
            }
            TagEnd::Table => {
                // tbody (if any), table, wrapper
                while self.stack.len() > 1 {
                    let top = self.top();
                    let tag = self.tag_of(top);
                    self.close();
                    if tag == "table" {
                        break;
                    }
                }
                self.close();
                self.table = None;
            }
            TagEnd::TableRow => {
                self.close();
            }
            TagEnd::BlockQuote(Some(_)) => {
                // body, callout
                self.close();
                self.close();
            }
            TagEnd::Link => {
                self.in_link = self.in_link.saturating_sub(1);
                self.close();
            }
            TagEnd::CodeBlock | TagEnd::HtmlBlock => {}
            _ => self.close(),
        }
    }

    /// Prose text: `marks` substitutions, then autolinks and file paths, then icon runs.
    fn prose(&mut self, text: &str) {
        let p = self.top();
        let in_image = self.dom.class_of(p) == "md-img";
        if self.in_link > 0 || in_image {
            spans::render_plain(self.dom, p, text);
            return;
        }
        let mut rest = text;
        'outer: while !rest.is_empty() {
            for m in self.marks {
                if m.t.is_empty() {
                    continue;
                }
                if let Some(at) = rest.find(&m.t) {
                    let (before, after) = rest.split_at(at);
                    linkify(self.dom, p, before);
                    spans::render(self.dom, p, std::slice::from_ref(m));
                    rest = &after[m.t.len()..];
                    continue 'outer;
                }
            }
            linkify(self.dom, p, rest);
            break;
        }
    }
}

fn title_case(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// The math delimiters pulldown-cmark doesn't know, as the ones it does (book: md Math):
/// `\(…\)` → `$…$`, `\[…\]` → `$$…$$`, and a `\begin{env}…\end{env}` starting a line →
/// `$$\begin{env}…\end{env}$$`. Code spans and fences, `$` math and escaped characters are left
/// as they are, as is an indented code block.
fn math_delimiters(src: &str) -> std::borrow::Cow<'_, str> {
    use std::borrow::Cow;
    if !(src.contains("\\(") || src.contains("\\[") || src.contains("\\begin{"))
        || src.starts_with("    ")
        || src.starts_with('\t')
    {
        return Cow::Borrowed(src);
    }
    let b = src.as_bytes();
    let run = |at: usize, c: u8| b[at..].iter().take_while(|&&x| x == c).count();
    let line_start = |at: usize| {
        src[..at]
            .rsplit('\n')
            .next()
            .is_none_or(|l| l.trim().is_empty())
    };
    let mut out = String::new();
    let (mut i, mut copied) = (0, 0);
    while i < b.len() {
        match b[i] {
            b'`' => {
                // A code span or fence: to the closing run of the same length.
                let n = run(i, b'`');
                let mut j = i + n;
                i = loop {
                    match b[j..].iter().position(|&x| x == b'`') {
                        Some(k) => {
                            let at = j + k;
                            let m = run(at, b'`');
                            if m == n {
                                break at + m;
                            }
                            j = at + m;
                        }
                        None => break i + n,
                    }
                };
            }
            b'$' if b.get(i + 1) == Some(&b'$') => {
                i = match src[i + 2..].find("$$") {
                    Some(k) => i + 2 + k + 2,
                    None => i + 2,
                };
            }
            b'$' => {
                // `$…$` math only as pulldown-cmark reads it: the opening `$` not followed by
                // whitespace, the closing one not preceded by whitespace nor followed by a
                // digit, within the paragraph. A lone `$` (a price) skips only itself, so a
                // `\(…\)` after it still converts.
                let mut close = None;
                if b.get(i + 1).is_some_and(|c| !c.is_ascii_whitespace()) {
                    let mut j = i + 1;
                    while let Some(k) = src[j..].find('$') {
                        let at = j + k;
                        if src[i + 1..at].contains("\n\n") {
                            break;
                        }
                        let after = b.get(at + 1).copied();
                        if !b[at - 1].is_ascii_whitespace()
                            && b[at - 1] != b'\\'
                            && !after.is_some_and(|c| c.is_ascii_digit() || c == b'$')
                        {
                            close = Some(at + 1);
                            break;
                        }
                        j = at + 1;
                    }
                }
                i = close.unwrap_or(i + 1);
            }
            b'\\' => match b.get(i + 1) {
                Some(&o @ (b'(' | b'[')) => {
                    let (close, fence) = if o == b'(' {
                        ("\\)", "$")
                    } else {
                        ("\\]", "$$")
                    };
                    if let Some(k) = src[i + 2..].find(close) {
                        let end = i + 2 + k;
                        let inner = src[i + 2..end].trim();
                        if !inner.is_empty() && !inner.contains('$') {
                            out.push_str(&src[copied..i]);
                            out.push_str(fence);
                            out.push_str(inner);
                            out.push_str(fence);
                            i = end + 2;
                            copied = i;
                            continue;
                        }
                    }
                    i += 2;
                }
                Some(b'b') if src[i..].starts_with("\\begin{") && line_start(i) => {
                    let name = src[i + 7..].split('}').next().unwrap_or("");
                    let end = format!("\\end{{{name}}}");
                    let valid = !name.is_empty()
                        && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'*');
                    match src[i..].find(&end).filter(|_| valid) {
                        Some(k) => {
                            let stop = i + k + end.len();
                            out.push_str(&src[copied..i]);
                            out.push_str("$$");
                            out.push_str(&src[i..stop]);
                            out.push_str("$$");
                            i = stop;
                            copied = i;
                        }
                        None => i += 1,
                    }
                }
                Some(_) => i += 2,
                None => i += 1,
            },
            _ => i += 1,
        }
    }
    if copied == 0 {
        return Cow::Borrowed(src);
    }
    out.push_str(&src[copied..]);
    Cow::Owned(out)
}

fn is_hex_color(t: &str) -> bool {
    let Some(h) = t.strip_prefix('#') else {
        return false;
    };
    matches!(h.len(), 3 | 4 | 6 | 8) && h.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A word that is a URL, an email or a file path (a separator or position plus an extension).
fn link_target(word: &str) -> Option<String> {
    let w = word.trim_end_matches(['.', ',', ';', ':', ')', '!', '?', '"', '\'']);
    if w.starts_with("http://") || w.starts_with("https://") {
        return (w.len() > 8).then(|| w.to_owned());
    }
    if let Some(rest) = w.strip_prefix("www.") {
        return (rest.contains('.')).then(|| format!("https://{w}"));
    }
    if let Some(at) = w.find('@') {
        let (user, host) = (&w[..at], &w[at + 1..]);
        if !user.is_empty()
            && host.contains('.')
            && !host.ends_with('.')
            && user
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b))
            && host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".-".contains(&b))
        {
            return Some(format!("mailto:{w}"));
        }
        return None;
    }
    // Paths: a separator or a :line position, plus a file extension.
    let base = w.split(':').next().unwrap_or(w);
    let has_pos = w.len() > base.len()
        && w[base.len() + 1..]
            .split(['-', ':'])
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    let has_sep = base.contains('/');
    let name = base.rsplit('/').next().unwrap_or(base);
    let ext_ok = name.rsplit_once('.').is_some_and(|(n, e)| {
        !n.is_empty()
            && (1..=8).contains(&e.len())
            && e.bytes().all(|b| b.is_ascii_alphanumeric())
            && e.bytes().any(|b| b.is_ascii_alphabetic())
    });
    let chars_ok = base
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"/._-~@+".contains(&b));
    ((has_sep || has_pos) && ext_ok && chars_ok && !base.contains("//")).then(|| w.to_owned())
}

fn linkify(dom: &mut Dom, parent: NodeId, text: &str) {
    let mut last = 0;
    let mut i = 0;
    let bytes = text.as_bytes();
    while i < bytes.len() {
        // Word boundaries are whitespace and a few brackets.
        let start = i;
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() && !b"()<>[]`".contains(&bytes[i])
        {
            i += 1;
        }
        if i > start {
            let word = &text[start..i];
            if let Some(href) = link_target(word) {
                let trimmed_len = word
                    .trim_end_matches(['.', ',', ';', ':', ')', '!', '?', '"', '\''])
                    .len();
                if start > last {
                    spans::render_plain(dom, parent, &text[last..start]);
                }
                let l = dom.child(parent, "span", "sf-link");
                dom.attr(l, "data-href", &href);
                dom.attr(l, "title", &format!("{href}  ⌘-click to open"));
                dom.text(l, &word[..trimmed_len]);
                last = start + trimmed_len;
            }
        }
        if i < bytes.len() {
            i += 1;
        }
    }
    if last < text.len() {
        spans::render_plain(dom, parent, &text[last..]);
    }
}

/// A fenced code block: language label, Copy, highlighted lines.
fn code_block(dom: &mut Dom, holder: NodeId, lang: Option<&str>, text: &str) {
    let text = text.strip_suffix('\n').unwrap_or(text);
    let head = dom.child(holder, "div", "code-head");
    dom.child_text(head, "span", "code-lang", lang.unwrap_or(""));
    let copy = dom.child_text(head, "button", "btn sm quiet code-copy", "Copy");
    dom.attr(copy, "data-copy", "code");
    if lang == Some("mermaid") {
        dom.set_class(holder, "code mermaid");
    }
    let pre = dom.child(holder, "pre", "");
    let code = dom.child(pre, "code", "");
    highlight_into(dom, code, lang, None, text);
}

/// Highlighted lines as `span` lines with `tk-*` runs.
pub(crate) fn highlight_into(
    dom: &mut Dom,
    parent: NodeId,
    lang: Option<&str>,
    path: Option<&str>,
    text: &str,
) {
    match highlight::find(lang, path) {
        // Plain now, highlighted in the background and drawn in when in view.
        Some(syn) => {
            let mut hl = highlight::later(syn, text);
            let lines: Vec<&str> = text.split('\n').collect();
            for (i, line) in lines.iter().enumerate() {
                let l = dom.child(parent, "span", "ln");
                let newline = i + 1 < lines.len();
                dom.text(l, line);
                if newline {
                    dom.text(l, "\n");
                }
                hl.add(highlight::DeferredLine {
                    el: l,
                    idx: i,
                    text: (*line).to_owned(),
                    em: Vec::new(),
                    newline,
                });
            }
            hl.finish();
        }
        None => {
            dom.text(parent, text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_split() {
        let (r, defs) = split("# A\n\npara one\n\n- x\n- y\n\n[r]: http://x\n\n```rs\nfn\n```\n");
        let src = "# A\n\npara one\n\n- x\n- y\n\n[r]: http://x\n\n```rs\nfn\n```\n";
        let blocks: Vec<&str> = r.iter().map(|r| &src[r.clone()]).collect();
        assert_eq!(blocks.len(), 4, "{blocks:?}");
        assert!(blocks[3].starts_with("```"));
        assert!(defs.contains("[r]: http://x"));
    }

    #[test]
    fn links() {
        assert_eq!(
            link_target("https://a.b/c.").as_deref(),
            Some("https://a.b/c")
        );
        assert_eq!(
            link_target("src/main.rs:12").as_deref(),
            Some("src/main.rs:12")
        );
        assert_eq!(
            link_target("main.rs:12-20").as_deref(),
            Some("main.rs:12-20")
        );
        assert_eq!(link_target("a@b.io").as_deref(), Some("mailto:a@b.io"));
        assert_eq!(link_target("v1.2"), None);
        assert_eq!(link_target("and/or"), None);
        assert_eq!(link_target("main.rs"), None);
    }
}
