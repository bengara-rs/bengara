//! 端末に表を出すための、文字の幅の計算。

/// 端末に出したときの幅を数える。
///
/// 日本語や記号は 2 文字分、ASCII は 1 文字分として数えます。
/// 厳密な東アジア文字幅の表は持ちません（表の桁をそろえるだけが目的です）。
pub(crate) fn width(text: &str) -> usize {
    text.chars().map(char_width).sum()
}

fn char_width(c: char) -> usize {
    match c {
        // 組み合わせ文字（濁点など）は幅を持たない。
        '\u{0300}'..='\u{036F}' | '\u{3099}'..='\u{309C}' => 0,
        c if (c as u32) < 0x1100 => 1,
        // ここから先はおおむね全角。
        '\u{1100}'..='\u{115F}'   // ハングルの字母
        | '\u{2E80}'..='\u{303E}' // CJK の記号・句読点
        | '\u{3041}'..='\u{33FF}' // かな・ハングル・CJK の囲み文字
        | '\u{3400}'..='\u{4DBF}' // CJK 拡張 A
        | '\u{4E00}'..='\u{9FFF}' // CJK 統合漢字
        | '\u{A000}'..='\u{A4CF}' // イ文字
        | '\u{AC00}'..='\u{D7A3}' // ハングル
        | '\u{F900}'..='\u{FAFF}' // CJK 互換漢字
        | '\u{FE30}'..='\u{FE6F}' // CJK 互換の記号
        | '\u{FF00}'..='\u{FF60}' // 全角の英数字
        | '\u{FFE0}'..='\u{FFE6}' // 全角の記号
        | '\u{1F300}'..='\u{1F64F}' // 絵文字
        | '\u{1F900}'..='\u{1F9FF}'
        | '\u{20000}'..='\u{3FFFD}' => 2,
        _ => 1,
    }
}

/// 右に空白を足して、端末での幅を `to` にそろえる。
///
/// すでに `to` より広いときは、そのまま返します。
pub(crate) fn pad(text: &str, to: usize) -> String {
    let current = width(text);
    if current >= to {
        return text.to_string();
    }
    format!("{text}{}", " ".repeat(to - current))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asciiは1文字分() {
        assert_eq!(width("abc"), 3);
        assert_eq!(width(""), 0);
        assert_eq!(width("queue:work"), 10);
    }

    #[test]
    fn 日本語は2文字分() {
        assert_eq!(width("名前"), 4);
        assert_eq!(width("記事数の集計"), 12);
        // j a 空白 で 3、と 日 本 語 で 8。
        assert_eq!(width("ja と日本語"), 11);
    }

    #[test]
    fn 幅をそろえる() {
        assert_eq!(pad("ab", 5), "ab   ");
        assert_eq!(pad("名前", 6), "名前  ");
        // すでに広いときはそのまま。
        assert_eq!(pad("名前", 2), "名前");
        assert_eq!(pad("", 3), "   ");
    }

    #[test]
    fn そろえた後の幅は同じ() {
        for name in ["生存の記録", "記事数の集計", "ab", "再設定トークンの掃除"]
        {
            assert_eq!(width(&pad(name, 20)), 20, "{name}");
        }
    }
}
