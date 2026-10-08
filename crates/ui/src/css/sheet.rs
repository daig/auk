//! Stylesheet parsing: rules, declarations, `@media`, `@keyframes`, nesting, and Tern's
//! accept/reject rules with its error format.

use super::properties::{DeclError, Prop, Restrictions, SV, parse_declaration};
use super::selector::{Impl, parse_selector_list, uses_unsupported};
use crate::dom::Atom;
use cssparser::{
    AtRuleParser, CowRcStr, DeclarationParser, ParseError, Parser, ParserState,
    QualifiedRuleParser, RuleBodyItemParser, RuleBodyParser, SourceLocation, StyleSheetParser,
};
use selectors::parser::SelectorList;
use std::collections::HashMap;
use std::sync::Arc;

/// What a declaration sets.
#[derive(Clone, Debug, PartialEq)]
pub enum DeclProp {
    Long(Prop),
    /// A custom property; the value is its raw token text.
    Custom(Atom),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Decl {
    pub prop: DeclProp,
    pub value: SV,
    pub important: bool,
}

/// A declaration block.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DeclBlock {
    pub decls: Vec<Decl>,
}

/// A media query from Tern's accepted subset (§4.5).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MediaQuery {
    pub max_width: Option<f32>,
    pub max_height: Option<f32>,
    pub reduced_motion: bool,
    pub reduced_transparency: bool,
    pub more_contrast: bool,
}

/// What media queries are evaluated against.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MediaEnv {
    pub width: f32,
    pub height: f32,
    pub reduced_motion: bool,
    pub reduced_transparency: bool,
    pub more_contrast: bool,
}

impl MediaQuery {
    pub fn matches(&self, env: &MediaEnv) -> bool {
        self.max_width.is_none_or(|w| env.width <= w)
            && self.max_height.is_none_or(|h| env.height <= h)
            && (!self.reduced_motion || env.reduced_motion)
            && (!self.reduced_transparency || env.reduced_transparency)
            && (!self.more_contrast || env.more_contrast)
    }
    fn and(&self, other: &MediaQuery) -> MediaQuery {
        MediaQuery {
            max_width: match (self.max_width, other.max_width) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            },
            max_height: match (self.max_height, other.max_height) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            },
            reduced_motion: self.reduced_motion || other.reduced_motion,
            reduced_transparency: self.reduced_transparency || other.reduced_transparency,
            more_contrast: self.more_contrast || other.more_contrast,
        }
    }
}

#[derive(Clone, Debug)]
pub struct StyleRule {
    pub selectors: SelectorList<Impl>,
    pub block: Arc<DeclBlock>,
    pub media: Option<Arc<MediaQuery>>,
}

/// A parsed `@keyframes` rule: offsets (0..=1) with their declarations.
#[derive(Clone, Debug, Default)]
pub struct Keyframes {
    pub frames: Vec<(f32, Arc<DeclBlock>)>,
}

#[derive(Clone, Debug, Default)]
pub struct Stylesheet {
    pub rules: Vec<StyleRule>,
    pub keyframes: HashMap<String, Keyframes>,
    /// Errors in Tern's format, `<line>:<col>: <message> in `<snippet>``.
    pub errors: Vec<String>,
}

impl Stylesheet {
    /// Parse a sheet. `r` selects the program-sheet restrictions.
    pub fn parse(css: &str, r: Restrictions) -> Stylesheet {
        let mut sheet = Stylesheet::default();
        let mut parser = Parser::new(css);
        let mut top = TopParser {
            r,
            errors: Vec::new(),
            out: Vec::new(),
            keyframes: HashMap::new(),
            media: None,
        };
        let results: Vec<_> = StyleSheetParser::new(&mut parser, &mut top).collect();
        for res in results {
            if let Err((err, slice, loc)) = res
                && !matches!(
                    err.kind,
                    cssparser::ParseErrorKind::Custom(SheetError::Silent)
                )
            {
                top.errors
                    .push(format_error(loc, &message_for(&err), slice));
            }
        }
        // Rule-level errors (selectors, at-rules) come back after the declarations parsed inside
        // the rules: report them all in source order.
        top.errors.sort_by_key(|e| {
            let mut it = e
                .splitn(3, ':')
                .map(|n| n.parse::<u32>().unwrap_or(u32::MAX));
            (it.next().unwrap_or(u32::MAX), it.next().unwrap_or(u32::MAX))
        });
        sheet.rules = top.out;
        sheet.keyframes = top.keyframes;
        sheet.errors = top.errors;
        sheet
    }
}

/// Parse an inline `style` attribute.
pub fn parse_inline_style(css: &str, r: Restrictions) -> (DeclBlock, Vec<String>) {
    let mut parser = Parser::new(css);
    let mut errors = Vec::new();
    let block = parse_block_body(&mut parser, r, &mut errors, None, &mut Vec::new(), None);
    (block, errors)
}

#[derive(Clone, Debug, PartialEq)]
pub enum SheetError {
    Decl(String),
    Selector,
    AtRule(String),
    Media,
    Silent,
}

fn message_for(err: &ParseError<SheetError>) -> String {
    match &err.kind {
        cssparser::ParseErrorKind::Custom(SheetError::Decl(m)) => m.clone(),
        cssparser::ParseErrorKind::Custom(SheetError::Selector) => "unsupported selector".into(),
        cssparser::ParseErrorKind::Custom(SheetError::AtRule(name)) => {
            format!("unsupported at-rule `@{name}`")
        }
        cssparser::ParseErrorKind::Custom(SheetError::Media) => "unsupported media query".into(),
        cssparser::ParseErrorKind::Custom(SheetError::Silent) => String::new(),
        _ => "parse error".into(),
    }
}

fn format_error(loc: SourceLocation, msg: &str, snippet: &str) -> String {
    let snippet: String = snippet
        .trim()
        .trim_end_matches([';', '{', ' '])
        .chars()
        .take(80)
        .collect();
    let snippet = snippet.split_whitespace().collect::<Vec<_>>().join(" ");
    format!("{}:{}: {} in `{}`", loc.line + 1, loc.column, msg, snippet)
}

struct TopParser {
    r: Restrictions,
    errors: Vec<String>,
    out: Vec<StyleRule>,
    keyframes: HashMap<String, Keyframes>,
    media: Option<Arc<MediaQuery>>,
}

enum AtPrelude {
    Media(MediaQuery),
    Keyframes(String),
    FontFace,
}

fn parse_media_query<'i>(input: &mut Parser<'i>) -> Result<MediaQuery, ParseError<SheetError>> {
    let mut q = MediaQuery::default();
    let mut first = true;
    loop {
        if input.is_exhausted() {
            break;
        }
        if !first {
            input
                .expect_ident_matching("and")
                .map_err(|_| ParseError::custom(SheetError::Media))?;
        }
        first = false;
        // Media types are ignored.
        if let Ok(t) = input.try_parse(|i| i.expect_ident_cloned()) {
            match t.to_ascii_lowercase().as_str() {
                "screen" | "all" | "only" => continue,
                _ => return Err(ParseError::custom(SheetError::Media)),
            }
        }
        input
            .expect_parenthesis_block()
            .map_err(|_| ParseError::custom(SheetError::Media))?;
        let feature =
            input.parse_nested_block(|i| -> Result<MediaQuery, ParseError<SheetError>> {
                let name = i.expect_ident_cloned()?.to_ascii_lowercase();
                let mut f = MediaQuery::default();
                if i.is_exhausted() {
                    match name.as_str() {
                        "prefers-reduced-motion" => f.reduced_motion = true,
                        "prefers-reduced-transparency" => f.reduced_transparency = true,
                        "prefers-contrast" => f.more_contrast = true,
                        _ => return Err(ParseError::custom(SheetError::Media)),
                    }
                    return Ok(f);
                }
                i.expect_colon()?;
                match name.as_str() {
                    "max-width" | "max-height" => {
                        let v = match i.next()?.clone() {
                            cssparser::Token::Dimension {
                                value, ref unit, ..
                            } if unit.eq_ignore_ascii_case("px") => value,
                            cssparser::Token::Number { value: 0.0, .. } => 0.0,
                            _ => return Err(ParseError::custom(SheetError::Media)),
                        };
                        if name == "max-width" {
                            f.max_width = Some(v)
                        } else {
                            f.max_height = Some(v)
                        }
                    }
                    "prefers-reduced-motion" => {
                        i.expect_ident_matching("reduce")
                            .map_err(|_| ParseError::custom(SheetError::Media))?;
                        f.reduced_motion = true;
                    }
                    "prefers-reduced-transparency" => {
                        i.expect_ident_matching("reduce")
                            .map_err(|_| ParseError::custom(SheetError::Media))?;
                        f.reduced_transparency = true;
                    }
                    "prefers-contrast" => {
                        let v = i.expect_ident_cloned()?.to_ascii_lowercase();
                        if v != "more" {
                            return Err(ParseError::custom(SheetError::Media));
                        }
                        f.more_contrast = true;
                    }
                    _ => return Err(ParseError::custom(SheetError::Media)),
                }
                Ok(f)
            })?;
        q = q.and(&feature);
    }
    Ok(q)
}

impl<'i> AtRuleParser<'i> for TopParser {
    type Prelude = AtPrelude;
    type AtRule = ();
    type Error = SheetError;

    fn parse_prelude<'t>(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i>,
    ) -> Result<AtPrelude, ParseError<SheetError>> {
        match name.to_ascii_lowercase().as_str() {
            "media" => Ok(AtPrelude::Media(parse_media_query(input)?)),
            "keyframes" | "-webkit-keyframes" => {
                let n = match input.next()?.clone() {
                    cssparser::Token::Ident(s) | cssparser::Token::QuotedString(s) => s.to_string(),
                    _ => return Err(ParseError::custom(SheetError::AtRule(name.to_string()))),
                };
                Ok(AtPrelude::Keyframes(n))
            }
            "font-face" => Ok(AtPrelude::FontFace),
            "import" => Err(ParseError::custom(SheetError::Decl(
                "@import is not supported".into(),
            ))),
            n => Err(ParseError::custom(SheetError::AtRule(n.to_string()))),
        }
    }

    fn parse_block<'t>(
        &mut self,
        prelude: AtPrelude,
        _start: &ParserState,
        input: &mut Parser<'i>,
    ) -> Result<(), ParseError<SheetError>> {
        match prelude {
            AtPrelude::FontFace => {
                while input.next().is_ok() {}
                Ok(())
            }
            AtPrelude::Media(q) => {
                let outer = self.media.clone();
                let q = match &outer {
                    Some(o) => o.and(&q),
                    None => q,
                };
                self.media = Some(Arc::new(q));
                let results: Vec<_> = StyleSheetParser::new(input, self).collect();
                for res in results {
                    if let Err((err, slice, loc)) = res
                        && !matches!(
                            err.kind,
                            cssparser::ParseErrorKind::Custom(SheetError::Silent)
                        )
                    {
                        self.errors
                            .push(format_error(loc, &message_for(&err), slice));
                    }
                }
                self.media = outer;
                Ok(())
            }
            AtPrelude::Keyframes(name) => {
                let mut kf = Keyframes::default();
                let mut kp = KeyframeParser {
                    r: self.r,
                    errors: Vec::new(),
                };
                let results: Vec<_> = StyleSheetParser::new(input, &mut kp).collect();
                for res in results {
                    match res {
                        Ok(frames) => kf.frames.extend(frames),
                        Err((err, slice, loc)) => {
                            kp.errors.push(format_error(loc, &message_for(&err), slice))
                        }
                    }
                }
                self.errors.extend(kp.errors);
                kf.frames.sort_by(|a, b| a.0.total_cmp(&b.0));
                self.keyframes.insert(name, kf);
                Ok(())
            }
        }
    }

    fn rule_without_block(&mut self, _prelude: AtPrelude, _start: &ParserState) -> Result<(), ()> {
        Err(())
    }
}

impl<'i> QualifiedRuleParser<'i> for TopParser {
    type Prelude = SelectorList<Impl>;
    type QualifiedRule = ();
    type Error = SheetError;

    fn parse_prelude<'t>(
        &mut self,
        input: &mut Parser<'i>,
    ) -> Result<SelectorList<Impl>, ParseError<SheetError>> {
        let start = input.position();
        let parsed = parse_selector_list(input);
        let text = input.slice_from(start);
        match parsed {
            Some(list) if !(self.r.tern_units && uses_unsupported(text)) => Ok(list),
            _ => {
                while input.next().is_ok() {}
                Err(ParseError::custom(SheetError::Selector))
            }
        }
    }

    fn parse_block<'t>(
        &mut self,
        prelude: SelectorList<Impl>,
        _start: &ParserState,
        input: &mut Parser<'i>,
    ) -> Result<(), ParseError<SheetError>> {
        let mut nested = Vec::new();
        let block = parse_block_body(
            input,
            self.r,
            &mut self.errors,
            Some(&prelude),
            &mut nested,
            self.media.clone(),
        );
        self.out.push(StyleRule {
            selectors: prelude,
            block: Arc::new(block),
            media: self.media.clone(),
        });
        self.out.extend(nested);
        Ok(())
    }
}

/// Parses the declarations (and nested rules) of one block.
struct BlockParser<'a> {
    r: Restrictions,
    parent: Option<&'a SelectorList<Impl>>,
    nested: &'a mut Vec<StyleRule>,
    media: Option<Arc<MediaQuery>>,
    errors: Vec<String>,
}

fn parse_block_body(
    input: &mut Parser<'_>,
    r: Restrictions,
    errors: &mut Vec<String>,
    parent: Option<&SelectorList<Impl>>,
    nested: &mut Vec<StyleRule>,
    media: Option<Arc<MediaQuery>>,
) -> DeclBlock {
    let mut bp = BlockParser {
        r,
        parent,
        nested,
        media,
        errors: Vec::new(),
    };
    let mut block = DeclBlock::default();
    let results: Vec<_> = RuleBodyParser::new(input, &mut bp).collect();
    for res in results {
        match res {
            Ok(BodyItem::Decls(d)) => block.decls.extend(d),
            Ok(BodyItem::Rule) => {}
            Err((err, slice, loc)) => {
                if !matches!(
                    err.kind,
                    cssparser::ParseErrorKind::Custom(SheetError::Silent)
                ) {
                    bp.errors.push(format_error(loc, &message_for(&err), slice));
                }
            }
        }
    }
    errors.append(&mut bp.errors);
    block
}

enum BodyItem {
    Decls(Vec<Decl>),
    Rule,
}

/// Split a raw declaration value into its value and `!important`.
fn split_important(raw: &str) -> (&str, bool) {
    let t = raw.trim_end();
    if let Some(i) = t.rfind('!') {
        let tail = t[i + 1..].trim();
        if tail.eq_ignore_ascii_case("important") {
            return (t[..i].trim_end(), true);
        }
    }
    (t, false)
}

impl<'i> DeclarationParser<'i> for BlockParser<'_> {
    type Declaration = BodyItem;
    type Error = SheetError;

    fn parse_value<'t>(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i>,
        _start: &ParserState,
    ) -> Result<BodyItem, ParseError<SheetError>> {
        let start = input.position();
        while input.next_including_whitespace_and_comments().is_ok() {}
        let raw = input.slice_from(start);
        let (value, important) = split_important(raw);
        if name.starts_with("--") {
            if self.r.no_url && value.to_ascii_lowercase().contains("url(") {
                return Err(ParseError::custom(SheetError::Decl(
                    DeclError::Url.message(&name),
                )));
            }
            return Ok(BodyItem::Decls(vec![Decl {
                prop: DeclProp::Custom(Atom::new(&name)),
                value: SV::Str(Arc::from(value.trim())),
                important,
            }]));
        }
        match parse_declaration(&name, value, self.r) {
            Ok(list) => Ok(BodyItem::Decls(
                list.into_iter()
                    .map(|(p, v)| Decl {
                        prop: DeclProp::Long(p),
                        value: v,
                        important,
                    })
                    .collect(),
            )),
            Err(DeclError::Silent) => Err(ParseError::custom(SheetError::Silent)),
            Err(e) => Err(ParseError::custom(SheetError::Decl(e.message(&name)))),
        }
    }
}

impl<'i> AtRuleParser<'i> for BlockParser<'_> {
    type Prelude = MediaQuery;
    type AtRule = BodyItem;
    type Error = SheetError;

    fn parse_prelude<'t>(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i>,
    ) -> Result<MediaQuery, ParseError<SheetError>> {
        if name.eq_ignore_ascii_case("media") && self.parent.is_some() {
            return parse_media_query(input);
        }
        Err(ParseError::custom(SheetError::AtRule(name.to_string())))
    }

    fn parse_block<'t>(
        &mut self,
        q: MediaQuery,
        _start: &ParserState,
        input: &mut Parser<'i>,
    ) -> Result<BodyItem, ParseError<SheetError>> {
        let media = Some(Arc::new(match &self.media {
            Some(m) => m.and(&q),
            None => q,
        }));
        let mut nested = Vec::new();
        let mut errors = Vec::new();
        let block = parse_block_body(
            input,
            self.r,
            &mut errors,
            self.parent,
            &mut nested,
            media.clone(),
        );
        self.errors.extend(errors);
        if let Some(parent) = self.parent
            && !block.decls.is_empty()
        {
            self.nested.push(StyleRule {
                selectors: parent.clone(),
                block: Arc::new(block),
                media,
            });
        }
        self.nested.extend(nested);
        Ok(BodyItem::Rule)
    }
}

impl<'i> QualifiedRuleParser<'i> for BlockParser<'_> {
    type Prelude = SelectorList<Impl>;
    type QualifiedRule = BodyItem;
    type Error = SheetError;

    fn parse_prelude<'t>(
        &mut self,
        input: &mut Parser<'i>,
    ) -> Result<SelectorList<Impl>, ParseError<SheetError>> {
        let start = input.position();
        while input.next().is_ok() {}
        let text = input.slice_from(start).trim().to_string();
        let Some(parent) = self.parent else {
            return Err(ParseError::custom(SheetError::Selector));
        };
        // A nested selector without `&` is relative to the parent as a descendant.
        let text = if text.contains('&') {
            text
        } else {
            text.split(',')
                .map(|s| format!("& {}", s.trim()))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let mut p = Parser::new(&text);
        match parse_selector_list(&mut p) {
            Some(list) if !(self.r.tern_units && uses_unsupported(&text)) => {
                Ok(list.replace_parent_selector(parent))
            }
            _ => Err(ParseError::custom(SheetError::Selector)),
        }
    }

    fn parse_block<'t>(
        &mut self,
        prelude: SelectorList<Impl>,
        _start: &ParserState,
        input: &mut Parser<'i>,
    ) -> Result<BodyItem, ParseError<SheetError>> {
        let mut nested = Vec::new();
        let mut errors = Vec::new();
        let block = parse_block_body(
            input,
            self.r,
            &mut errors,
            Some(&prelude),
            &mut nested,
            self.media.clone(),
        );
        self.errors.extend(errors);
        self.nested.push(StyleRule {
            selectors: prelude,
            block: Arc::new(block),
            media: self.media.clone(),
        });
        self.nested.extend(nested);
        Ok(BodyItem::Rule)
    }
}

impl<'i> RuleBodyItemParser<'i, BodyItem, SheetError> for BlockParser<'_> {
    fn parse_declarations(&self) -> bool {
        true
    }
    fn parse_qualified(&self) -> bool {
        self.parent.is_some()
    }
}

struct KeyframeParser {
    r: Restrictions,
    errors: Vec<String>,
}

impl<'i> AtRuleParser<'i> for KeyframeParser {
    type Prelude = ();
    type AtRule = Vec<(f32, Arc<DeclBlock>)>;
    type Error = SheetError;
}

impl<'i> QualifiedRuleParser<'i> for KeyframeParser {
    type Prelude = Vec<f32>;
    type QualifiedRule = Vec<(f32, Arc<DeclBlock>)>;
    type Error = SheetError;

    fn parse_prelude<'t>(
        &mut self,
        input: &mut Parser<'i>,
    ) -> Result<Vec<f32>, ParseError<SheetError>> {
        input.parse_comma_separated(|i| match i.next()?.clone() {
            cssparser::Token::Ident(s) if s.eq_ignore_ascii_case("from") => Ok(0.0),
            cssparser::Token::Ident(s) if s.eq_ignore_ascii_case("to") => Ok(1.0),
            cssparser::Token::Percentage { unit_value, .. } => Ok(unit_value),
            _ => Err(ParseError::unexpected_token()),
        })
    }

    fn parse_block<'t>(
        &mut self,
        prelude: Vec<f32>,
        _start: &ParserState,
        input: &mut Parser<'i>,
    ) -> Result<Vec<(f32, Arc<DeclBlock>)>, ParseError<SheetError>> {
        let block = Arc::new(parse_block_body(
            input,
            self.r,
            &mut self.errors,
            None,
            &mut Vec::new(),
            None,
        ));
        Ok(prelude.into_iter().map(|o| (o, block.clone())).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rules_and_reports_tern_errors() {
        let css = ".a { color: red; width: 10px }\n.b { background: url(x.png); position: fixed; foo: 1 }\n@import 'x';\n@font-face { font-family: X; src: url(x) }\n.c ~ .d { color: red }";
        let sheet = Stylesheet::parse(css, Restrictions::PROGRAM);
        assert_eq!(sheet.rules.len(), 2);
        assert_eq!(sheet.rules[0].block.decls.len(), 2);
        assert!(sheet.rules[1].block.decls.is_empty());
        let errs = sheet.errors.join("; ");
        assert!(
            errs.contains("2:6: url() is not allowed in `background: url(x.png)`"),
            "{errs}"
        );
        assert!(errs.contains("position: fixed is not allowed"), "{errs}");
        assert!(errs.contains("unknown property `foo`"), "{errs}");
        assert!(errs.contains("@import is not supported"), "{errs}");
        assert!(errs.contains("unsupported selector"), "{errs}");
        assert!(!errs.contains("font-face"), "{errs}");
    }

    #[test]
    fn nesting_and_media() {
        let css = ".a { color: red; .b { color: blue } &:hover { color: green } @media (max-width: 500px) { color: black } }";
        let sheet = Stylesheet::parse(css, Restrictions::NONE);
        assert_eq!(sheet.rules.len(), 4, "{:?}", sheet.errors);
        assert!(
            sheet
                .rules
                .iter()
                .any(|r| r.media.as_ref().is_some_and(|m| m.max_width == Some(500.0)))
        );
        let bad = Stylesheet::parse(
            "@media (min-width: 10px) { .a { color: red } }",
            Restrictions::NONE,
        );
        assert!(bad.rules.is_empty());
        assert!(
            bad.errors[0].contains("unsupported media query"),
            "{:?}",
            bad.errors
        );
    }

    #[test]
    fn important_and_custom_properties() {
        let sheet = Stylesheet::parse(
            ".a { --x: 4px; width: var(--x) !important }",
            Restrictions::NONE,
        );
        let decls = &sheet.rules[0].block.decls;
        assert!(matches!(decls[0].prop, DeclProp::Custom(_)));
        assert!(decls[1].important);
        assert!(matches!(decls[1].value, SV::Unparsed { .. }));
    }
}
