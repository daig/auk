//! The TSP engine's [`SheetChecker`] hook, implemented with the UI engine's stylesheet parser, so
//! programs get Tern-format CSS errors (`<line>:<col>: <message> in <snippet>`).

use auk_tsp::SheetChecker;
use auk_ui::css::properties::Restrictions;
use auk_ui::css::sheet::Stylesheet;

/// Checks program sheets with the program-sheet rules: `url()` and `position: fixed` dropped
/// and reported, `@import` reported, Tern's unit set, parse errors.
#[derive(Clone, Copy, Debug, Default)]
pub struct UiSheetChecker;

impl SheetChecker for UiSheetChecker {
    fn check(&self, css: &str) -> Vec<String> {
        Stylesheet::parse(css, Restrictions::PROGRAM).errors
    }
}

/// A boxed checker for [`auk_tsp::TspPane::with`].
pub fn sheet_checker() -> Box<dyn SheetChecker> {
    Box::new(UiSheetChecker)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn program_sheet_errors() {
        let c = UiSheetChecker;
        assert!(c.check(".a { color: red }").is_empty());
        let e =
            c.check(".a { background: url(x.png); position: fixed; width: 2rem }\n@import 'x';");
        assert!(e.len() >= 3, "{e:?}");
        assert!(
            e.iter()
                .all(|m| m.split(':').next().unwrap().parse::<u32>().is_ok()),
            "{e:?}"
        );
    }
}
