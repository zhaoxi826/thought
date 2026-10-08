//! 文本处理：规范化、内容 hash、检索用分词。

use sha2::{Digest, Sha256};

/// 规范化：去首尾空白、把连续空白折叠成一个空格。用于内容去重。
pub fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 内容 hash（sha256 前 16 字节的十六进制），用作记录 id：同内容必同 id。
pub fn content_hash(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    digest[..16].iter().map(|b| format!("{b:02x}")).collect()
}

/// 实体名规范化：小写 + 折叠空白。实体以此去重。
pub fn entity_key(name: &str) -> String {
    normalize(name).to_lowercase()
}

/// 关系 id：由 (主语, 谓词, 宾语) 的规范化形式决定。
pub fn relation_key(subject: &str, predicate: &str, object: &str) -> String {
    content_hash(&format!(
        "{}|{}|{}",
        entity_key(subject),
        entity_key(predicate),
        entity_key(object)
    ))
}

fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{4E00}'..='\u{9FFF}'   // CJK 统一表意文字
        | '\u{3400}'..='\u{4DBF}' // 扩展 A
        | '\u{3040}'..='\u{30FF}' // 平假名 / 片假名
        | '\u{AC00}'..='\u{D7AF}' // 韩文音节
        | '\u{F900}'..='\u{FAFF}')
}

/// 检索分词：拉丁字母 / 数字按词切分并小写；CJK 连续段切成字二元组
/// （单字段保留单字）。结果以空格拼接，配合 SurrealDB 的 `blank` 分词器建 BM25 索引。
///
/// 不依赖词典，中英混排都能工作；二元组对中文短语召回足够，代价是索引略大。
pub fn search_terms(text: &str) -> String {
    let mut terms: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut cjk: Vec<char> = Vec::new();

    let flush_word = |word: &mut String, terms: &mut Vec<String>| {
        if !word.is_empty() {
            terms.push(word.to_lowercase());
            word.clear();
        }
    };
    let flush_cjk = |cjk: &mut Vec<char>, terms: &mut Vec<String>| {
        match cjk.len() {
            0 => {}
            1 => terms.push(cjk[0].to_string()),
            _ => terms.extend(cjk.windows(2).map(|w| w.iter().collect::<String>())),
        }
        cjk.clear();
    };

    for c in text.chars() {
        if is_cjk(c) {
            flush_word(&mut word, &mut terms);
            cjk.push(c);
        } else if c.is_alphanumeric() {
            flush_cjk(&mut cjk, &mut terms);
            word.push(c);
        } else {
            flush_word(&mut word, &mut terms);
            flush_cjk(&mut cjk, &mut terms);
        }
    }
    flush_word(&mut word, &mut terms);
    flush_cjk(&mut cjk, &mut terms);
    terms.join(" ")
}

/// 查询分词：同 [`search_terms`]，但去重（避免同一词在 OR 查询里重复加权）。
pub fn query_terms(text: &str) -> String {
    let mut seen = std::collections::HashSet::new();
    search_terms(text)
        .split(' ')
        .filter(|t| !t.is_empty() && seen.insert(*t))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_mixed_text() {
        assert_eq!(search_terms("我喜欢 Rust 2024!"), "我喜 喜欢 rust 2024");
        assert_eq!(search_terms("在北京"), "在北 北京");
        assert_eq!(search_terms("a 好 b"), "a 好 b");
        assert_eq!(query_terms("北京 北京"), "北京");
    }

    #[test]
    fn keys_are_normalized() {
        assert_eq!(normalize("  a \n b  "), "a b");
        assert_eq!(entity_key(" Alice  Smith"), "alice smith");
        assert_eq!(relation_key("Alice", "likes", "Bob"), relation_key("alice", " likes", "BOB"));
        assert_ne!(relation_key("a", "b", "c"), relation_key("c", "b", "a"));
        assert_eq!(content_hash("x").len(), 32);
    }
}
