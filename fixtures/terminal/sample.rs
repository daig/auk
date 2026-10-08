//! A sample file for terminal recordings: code, wide characters and emoji.

use std::collections::BTreeMap;

/// 一个简单的计数器 (a simple counter), with a 名前 and a 😀 in its docs.
#[derive(Debug, Default, Clone)]
pub struct Counter {
    name: String,
    counts: BTreeMap<String, u64>,
}

impl Counter {
    pub fn new(name: &str) -> Self {
        Counter { name: name.to_owned(), counts: BTreeMap::new() }
    }

    /// Adds one to `word`. 単語を数える。
    pub fn add(&mut self, word: &str) -> u64 {
        let n = self.counts.entry(word.to_owned()).or_insert(0);
        *n += 1;
        *n
    }

    pub fn report(&self) -> String {
        let mut out = format!("{}:\n", self.name);
        for (word, n) in &self.counts {
            out.push_str(&format!("\t{word:<12} {n:>6}\n"));
        }
        out
    }
}

fn main() {
    let mut c = Counter::new("文字 counter 🚀");
    for word in "the quick brown fox jumps over the lazy dog the end".split(' ') {
        c.add(word);
    }
    // A long line that runs past the right edge of an 80-column terminal, to make vim scroll sideways when the cursor goes there.
    let wide = "全角文字の行：ＡＢＣＤＥＦＧ　ｈｉｊｋ，１２３４５６７８９０。한국어 텍스트도 있습니다。";
    println!("{}\n{wide}", c.report());
    let emoji = ["🦀", "👍🏽", "🇯🇵", "👩‍💻", "1️⃣", "❤️"];
    for e in emoji {
        print!("[{e}]");
    }
    println!();
}
