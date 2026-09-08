//! Bounded conversion from Servo's untrusted JavaScript result into a
//! native, text-only reader model.
//!
//! The extractor runs in the page's main JavaScript world, so neither
//! its behavior nor its output is trusted. Rust applies independent
//! shape, block-count and character limits before egui sees any text.

use std::collections::HashMap;

use servo::JSValue;
use url::Url;

pub(crate) const MAX_READER_BLOCKS: usize = 384;
pub(crate) const MAX_READER_BLOCK_CHARS: usize = 4_000;
pub(crate) const MAX_READER_TOTAL_CHARS: usize = 120_000;
const MAX_READER_TITLE_CHARS: usize = 240;
const MAX_READER_BYLINE_CHARS: usize = 160;
const MIN_READER_TOTAL_CHARS: usize = 120;

/// The script returns only text and coarse block kinds. It never
/// returns or executes page HTML and never performs another fetch.
pub(crate) const EXTRACTION_SCRIPT: &str = r#"
(() => {
  const MAX_ROOT_SCAN = 512;
  const MAX_ELEMENTS = 2048;
  const MAX_TEXT_NODES = 8192;
  const MAX_TEXT_NODES_PER_BLOCK = 512;
  const MAX_BLOCKS = 384;
  const MAX_BLOCK_CHARS = 4000;
  const MAX_TOTAL_CHARS = 120000;
  const normalize = (value, limit, preserveLines = false) => {
    const bounded = (typeof value === 'string' ? value : '')
      .slice(0, Math.min(limit * 2, 8000));
    if (preserveLines) {
      return bounded
        .replace(/\r\n?/g, '\n')
        .replace(/[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/g, '')
        .slice(0, limit);
    }
    return bounded
      .replace(/[\u0000-\u001f\u007f\s]+/g, ' ')
      .trim()
      .slice(0, limit);
  };
  const hasToken = (value, token, limit) =>
    (` ${normalize(value, limit)} `).includes(` ${token} `);
  let textNodesLeft = MAX_TEXT_NODES;
  const boundedText = (node, limit, preserveLines = false) => {
    if (!node || limit <= 0 || textNodesLeft <= 0) return '';
    const walker = document.createTreeWalker(node, 4);
    let output = '';
    let visited = 0;
    let textNode;
    while (
      output.length < limit &&
      textNodesLeft > 0 &&
      visited < MAX_TEXT_NODES_PER_BLOCK &&
      (textNode = walker.nextNode())
    ) {
      visited += 1;
      textNodesLeft -= 1;
      const chunk = normalize(textNode.nodeValue, limit - output.length, preserveLines);
      if (chunk) {
        output += preserveLines ? chunk : `${output ? ' ' : ''}${chunk}`;
      }
    }
    return output.trim().slice(0, limit);
  };

  let root = document.body;
  if (!root) return { title: '', byline: '', direction: '', blocks: [] };
  const rootWalker = document.createTreeWalker(root, 1);
  for (let scanned = 0, node; scanned < MAX_ROOT_SCAN && (node = rootWalker.nextNode()); scanned += 1) {
    const tag = node.tagName;
    if (tag === 'ARTICLE' || tag === 'MAIN' || node.getAttribute?.('role') === 'main') {
      root = node;
      break;
    }
  }

  let title = '';
  let byline = '';
  const headChildren = document.head?.children;
  if (headChildren) {
    for (let index = 0; index < headChildren.length && index < 128; index += 1) {
      const node = headChildren[index];
      if (
        node.tagName === 'META' &&
        (node.getAttribute?.('name') === 'author' ||
          node.getAttribute?.('property') === 'article:author')
      ) {
        byline = normalize(node.getAttribute?.('content'), 160);
        break;
      }
    }
  }
  const direction = normalize(
    root.getAttribute?.('dir') || document.documentElement?.getAttribute?.('dir') || '',
    8,
  ).toLowerCase();

  const blocks = [];
  const seen = new Set();
  let total = 0;
  const excludedTags = new Set(['NAV', 'ASIDE', 'HEADER', 'FOOTER', 'FORM', 'DIALOG', 'SCRIPT', 'STYLE', 'NOSCRIPT']);
  const isExcluded = (node) => {
    let current = node;
    for (let depth = 0; current && depth < 32; depth += 1, current = current.parentElement) {
      if (
        excludedTags.has(current.tagName) ||
        current.hasAttribute?.('hidden') ||
        current.getAttribute?.('aria-hidden') === 'true'
      ) return true;
      if (current === root) break;
    }
    return false;
  };
  const walker = document.createTreeWalker(root, 1);
  for (let index = 0, node; index < MAX_ELEMENTS && (node = walker.nextNode()); index += 1) {
    if (blocks.length >= MAX_BLOCKS || total >= MAX_TOTAL_CHARS) break;
    if (isExcluded(node)) continue;
    const tag = node.tagName;
    const isHeading = tag === 'H1' || tag === 'H2' || tag === 'H3' || tag === 'H4';
    const isBlock = isHeading || tag === 'P' || tag === 'LI' || tag === 'BLOCKQUOTE' || tag === 'PRE';
    if (!isBlock) {
      if (!byline && (
        hasToken(node.getAttribute?.('rel'), 'author', 64) ||
        normalize(node.getAttribute?.('itemprop'), 32) === 'author' ||
        hasToken(node.getAttribute?.('class'), 'byline', 128)
      )) byline = boundedText(node, 160);
      continue;
    }
    const text = boundedText(
      node,
      Math.min(MAX_BLOCK_CHARS, MAX_TOTAL_CHARS - total),
      tag === 'PRE',
    );
    if (!text) continue;
    if (!title && tag === 'H1') title = text.slice(0, 240);
    if ((tag === 'H1' && text === title) || seen.has(text)) continue;
    if (text.length < 24 && !isHeading) continue;
    seen.add(text);
    let kind = 'paragraph';
    if (isHeading) kind = 'heading';
    else if (tag === 'BLOCKQUOTE') kind = 'quote';
    else if (tag === 'PRE') kind = 'code';
    blocks.push({ kind, text });
    total += text.length;
  }

  if (blocks.length === 0) {
    const fallback = boundedText(root, Math.min(MAX_BLOCK_CHARS, MAX_TOTAL_CHARS));
    if (fallback) blocks.push({ kind: 'paragraph', text: fallback });
  }
  if (!title) title = normalize(document.title, 240);
  return { title, byline, direction, blocks };
})()
"#;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReaderDirection {
    Auto,
    LeftToRight,
    RightToLeft,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReaderBlockKind {
    Heading,
    Paragraph,
    Quote,
    Code,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReaderBlock {
    pub(crate) kind: ReaderBlockKind,
    pub(crate) text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReaderArticle {
    pub(crate) title: String,
    pub(crate) byline: Option<String>,
    pub(crate) direction: ReaderDirection,
    pub(crate) blocks: Vec<ReaderBlock>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReaderError {
    EvaluationFailed,
    MalformedResult,
    NoArticle,
    TimedOut,
}

impl ReaderError {
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::EvaluationFailed => "The page could not be read.",
            Self::MalformedResult => "The page returned an invalid reader result.",
            Self::NoArticle => "No sufficiently long article text was found.",
            Self::TimedOut => "Reader extraction took too long; its evaluator is still stopping.",
        }
    }
}

pub(crate) fn reader_url_is_eligible(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https") && url.has_host()
}

pub(crate) fn article_from_js(value: JSValue) -> Result<ReaderArticle, ReaderError> {
    let JSValue::Object(mut object) = value else {
        return Err(ReaderError::MalformedResult);
    };

    let title = take_bounded_string(&mut object, "title", MAX_READER_TITLE_CHARS)
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| "Reader view".to_owned());
    let byline = take_bounded_string(&mut object, "byline", MAX_READER_BYLINE_CHARS)
        .filter(|byline| !byline.is_empty());
    let direction = match take_bounded_string(&mut object, "direction", 8).as_deref() {
        Some("rtl") => ReaderDirection::RightToLeft,
        Some("ltr") => ReaderDirection::LeftToRight,
        _ => ReaderDirection::Auto,
    };
    let Some(JSValue::Array(block_values)) = object.remove("blocks") else {
        return Err(ReaderError::MalformedResult);
    };

    let mut blocks = Vec::with_capacity(block_values.len().min(MAX_READER_BLOCKS));
    let mut total_chars = 0;
    for value in block_values.into_iter().take(MAX_READER_BLOCKS) {
        if total_chars >= MAX_READER_TOTAL_CHARS {
            break;
        }
        let JSValue::Object(mut block) = value else {
            continue;
        };
        let kind = match take_string(&mut block, "kind").as_deref() {
            Some("heading") => ReaderBlockKind::Heading,
            Some("quote") => ReaderBlockKind::Quote,
            Some("code") => ReaderBlockKind::Code,
            _ => ReaderBlockKind::Paragraph,
        };
        let remaining = MAX_READER_TOTAL_CHARS - total_chars;
        let limit = MAX_READER_BLOCK_CHARS.min(remaining);
        let Some(raw_text) = take_string(&mut block, "text") else {
            continue;
        };
        let text = normalize_text(&raw_text, limit, kind == ReaderBlockKind::Code);
        if text.is_empty() {
            continue;
        }
        total_chars += text.chars().count();
        blocks.push(ReaderBlock { kind, text });
    }

    if total_chars < MIN_READER_TOTAL_CHARS {
        return Err(ReaderError::NoArticle);
    }
    Ok(ReaderArticle {
        title,
        byline,
        direction,
        blocks,
    })
}

fn take_string(object: &mut HashMap<String, JSValue>, key: &str) -> Option<String> {
    match object.remove(key) {
        Some(JSValue::String(value)) => Some(value),
        _ => None,
    }
}

fn take_bounded_string(
    object: &mut HashMap<String, JSValue>,
    key: &str,
    max_chars: usize,
) -> Option<String> {
    take_string(object, key).map(|value| normalize_text(&value, max_chars, false))
}

fn normalize_text(input: &str, max_chars: usize, preserve_lines: bool) -> String {
    if preserve_lines {
        let mut output = String::with_capacity(input.len().min(max_chars));
        let mut output_chars = 0;
        let mut previous_was_carriage_return = false;
        for character in input.chars() {
            if output_chars >= max_chars {
                break;
            }
            match character {
                '\r' => {
                    output.push('\n');
                    output_chars += 1;
                    previous_was_carriage_return = true;
                }
                '\n' if previous_was_carriage_return => {
                    previous_was_carriage_return = false;
                }
                '\n' | '\t' => {
                    output.push(character);
                    output_chars += 1;
                    previous_was_carriage_return = false;
                }
                _ if character.is_control() => {
                    previous_was_carriage_return = false;
                }
                _ => {
                    output.push(character);
                    output_chars += 1;
                    previous_was_carriage_return = false;
                }
            }
        }
        return output.trim().to_owned();
    }

    let mut output = String::with_capacity(input.len().min(max_chars));
    let mut output_chars = 0;
    let mut pending_space = false;
    for character in input.chars() {
        if character.is_whitespace() || character.is_control() {
            pending_space = !output.is_empty();
            continue;
        }
        if pending_space {
            if output_chars >= max_chars {
                break;
            }
            output.push(' ');
            output_chars += 1;
            pending_space = false;
        }
        if output_chars >= max_chars {
            break;
        }
        output.push(character);
        output_chars += 1;
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn string(value: &str) -> JSValue {
        JSValue::String(value.to_owned())
    }

    fn block(kind: &str, text: &str) -> JSValue {
        JSValue::Object(HashMap::from([
            ("kind".to_owned(), string(kind)),
            ("text".to_owned(), string(text)),
        ]))
    }

    fn article(blocks: Vec<JSValue>) -> JSValue {
        JSValue::Object(HashMap::from([
            ("title".to_owned(), string(" A\n useful\t title ")),
            ("byline".to_owned(), string(" Ada\u{7} Lovelace ")),
            ("direction".to_owned(), string("rtl")),
            ("blocks".to_owned(), JSValue::Array(blocks)),
        ]))
    }

    #[test]
    fn parses_text_only_article_and_normalizes_controls() {
        let paragraph = "Grüße — 日本語 — 한국어 — مرحبا ".repeat(8);
        let parsed = article_from_js(article(vec![block("paragraph", &paragraph)])).unwrap();
        assert_eq!(parsed.title, "A useful title");
        assert_eq!(parsed.byline.as_deref(), Some("Ada Lovelace"));
        assert_eq!(parsed.direction, ReaderDirection::RightToLeft);
        assert_eq!(parsed.blocks[0].kind, ReaderBlockKind::Paragraph);
        assert!(parsed.blocks[0].text.contains("日本語"));
    }

    #[test]
    fn rejects_short_or_malformed_results() {
        assert_eq!(
            article_from_js(JSValue::String("not an object".to_owned())),
            Err(ReaderError::MalformedResult)
        );
        assert_eq!(
            article_from_js(article(vec![block("paragraph", "too short")])),
            Err(ReaderError::NoArticle)
        );
    }

    #[test]
    fn independently_caps_untrusted_blocks_and_text() {
        let long = "x".repeat(MAX_READER_BLOCK_CHARS + 500);
        let blocks = (0..MAX_READER_BLOCKS + 20)
            .map(|_| block("paragraph", &long))
            .collect();
        let parsed = article_from_js(article(blocks)).unwrap();
        assert!(parsed.blocks.len() <= MAX_READER_BLOCKS);
        assert!(parsed
            .blocks
            .iter()
            .all(|block| block.text.chars().count() <= MAX_READER_BLOCK_CHARS));
        assert!(
            parsed
                .blocks
                .iter()
                .map(|block| block.text.chars().count())
                .sum::<usize>()
                <= MAX_READER_TOTAL_CHARS
        );
    }

    #[test]
    fn code_blocks_keep_lines_while_still_removing_controls() {
        let code = "fn main() {\r\n\tprintln!(\"hello\");\u{7}\r\n}\n";
        let padding = "A sufficiently long article paragraph. ".repeat(4);
        let parsed = article_from_js(article(vec![
            block("code", code),
            block("paragraph", &padding),
        ]))
        .unwrap();
        assert_eq!(
            parsed.blocks[0].text,
            "fn main() {\n\tprintln!(\"hello\");\n}"
        );
    }

    #[test]
    fn only_http_documents_are_reader_eligible() {
        assert!(reader_url_is_eligible(
            &Url::parse("https://example.com/article").unwrap()
        ));
        assert!(reader_url_is_eligible(
            &Url::parse("http://example.com/article").unwrap()
        ));
        assert!(!reader_url_is_eligible(&Url::parse("about:blank").unwrap()));
        assert!(!reader_url_is_eligible(
            &Url::parse("data:text/plain,hello").unwrap()
        ));
    }
}
