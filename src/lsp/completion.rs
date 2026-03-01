//! Completion candidates for the LSP module.
//!
//! Given a set of open documents and a cursor position, returns symbol names
//! from all open documents that start with the prefix extracted from the
//! content at the cursor position (the "word before cursor").

use crate::parser::CodeSymbol;

// ---------------------------------------------------------------------------
// CompletionItem
// ---------------------------------------------------------------------------

/// A single completion candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionItem {
    /// The label shown in the completion list.
    pub label: String,
    /// Symbol kind string (e.g. "function", "struct").
    pub kind: String,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Return completion candidates from `all_symbols` whose names start with
/// `prefix` (case-sensitive prefix match).
///
/// Deduplicates by label so that the same name from multiple documents appears
/// only once. Results are sorted alphabetically.
pub fn complete(all_symbols: &[&[CodeSymbol]], prefix: &str) -> Vec<CompletionItem> {
    let mut seen = std::collections::HashSet::new();
    let mut items: Vec<CompletionItem> = Vec::new();

    for symbols in all_symbols {
        collect_completions(symbols, prefix, &mut seen, &mut items);
    }

    items.sort_by(|a, b| a.label.cmp(&b.label));
    items
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Walk a symbol list (including children) and collect completions for names
/// starting with `prefix`.
fn collect_completions(
    symbols: &[CodeSymbol],
    prefix: &str,
    seen: &mut std::collections::HashSet<String>,
    out: &mut Vec<CompletionItem>,
) {
    for sym in symbols {
        if sym.name.starts_with(prefix) && seen.insert(sym.name.clone()) {
            out.push(CompletionItem {
                label: sym.name.clone(),
                kind: sym.kind.to_string(),
            });
        }
        // Recurse into children (methods, fields, etc.)
        collect_completions(&sym.children, prefix, seen, out);
    }
}

/// Extract the word (identifier-like token) immediately before the cursor.
///
/// `content` is the full document text. `line` and `col` are 0-based.
/// Returns an empty string if the position is out of bounds or there is no
/// word before the cursor.
pub fn word_before_cursor(content: &str, line: u32, col: u32) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let line_str = match lines.get(line as usize) {
        Some(l) => *l,
        None => return String::new(),
    };

    // Take the portion of the line up to `col`.
    let col = col as usize;
    let prefix_chars: Vec<char> = line_str.chars().take(col).collect();

    // Walk backwards collecting identifier characters.
    let word: String = prefix_chars
        .iter()
        .rev()
        .take_while(|&&c| c.is_alphanumeric() || c == '_')
        .collect::<String>()
        .chars()
        .rev()
        .collect();

    word
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{CodeSymbol, SymbolKind, TextRange};

    fn sym(name: &str, kind: SymbolKind) -> CodeSymbol {
        CodeSymbol {
            name: name.to_string(),
            kind,
            range: TextRange {
                start_line: 0,
                start_column: 0,
                end_line: 1,
                end_column: 0,
            },
            detail: None,
            children: Vec::new(),
        }
    }

    #[test]
    fn test_complete_prefix_match() {
        let symbols = vec![
            sym("handle_request", SymbolKind::Function),
            sym("handle_response", SymbolKind::Function),
            sym("Payload", SymbolKind::Struct),
        ];
        let items = complete(&[symbols.as_slice()], "handle");
        assert_eq!(items.len(), 2);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"handle_request"));
        assert!(labels.contains(&"handle_response"));
    }

    #[test]
    fn test_complete_empty_prefix_returns_all() {
        let symbols = vec![
            sym("foo", SymbolKind::Function),
            sym("bar", SymbolKind::Function),
            sym("baz", SymbolKind::Struct),
        ];
        let items = complete(&[symbols.as_slice()], "");
        assert_eq!(items.len(), 3);
    }

    #[test]
    fn test_complete_no_match() {
        let symbols = vec![sym("foo", SymbolKind::Function)];
        let items = complete(&[symbols.as_slice()], "zzz");
        assert!(items.is_empty());
    }

    #[test]
    fn test_complete_deduplication_across_documents() {
        let doc1 = vec![sym("shared", SymbolKind::Function)];
        let doc2 = vec![sym("shared", SymbolKind::Function)];
        let items = complete(&[doc1.as_slice(), doc2.as_slice()], "shared");
        assert_eq!(items.len(), 1, "should deduplicate across documents");
    }

    #[test]
    fn test_complete_sorted_alphabetically() {
        let symbols = vec![
            sym("zebra", SymbolKind::Function),
            sym("alpha", SymbolKind::Function),
            sym("middle", SymbolKind::Function),
        ];
        let items = complete(&[symbols.as_slice()], "");
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, vec!["alpha", "middle", "zebra"]);
    }

    #[test]
    fn test_complete_includes_children() {
        let method = CodeSymbol {
            name: "do_work".to_string(),
            kind: SymbolKind::Method,
            range: TextRange {
                start_line: 2,
                start_column: 0,
                end_line: 4,
                end_column: 0,
            },
            detail: None,
            children: Vec::new(),
        };
        let class = CodeSymbol {
            name: "Worker".to_string(),
            kind: SymbolKind::Class,
            range: TextRange {
                start_line: 0,
                start_column: 0,
                end_line: 5,
                end_column: 0,
            },
            detail: None,
            children: vec![method],
        };

        let items = complete(&[std::slice::from_ref(&class)], "do");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "do_work");
        assert_eq!(items[0].kind, "method");
    }

    #[test]
    fn test_word_before_cursor_simple() {
        let content = "let my_var = 42;";
        // cursor at col 10 (after "my_var")
        let word = word_before_cursor(content, 0, 10);
        assert_eq!(word, "my_var");
    }

    #[test]
    fn test_word_before_cursor_partial() {
        let content = "fn handle_req(";
        // cursor at col 13 (after the full "handle_req", before "(")
        let word = word_before_cursor(content, 0, 13);
        assert_eq!(word, "handle_req");
    }

    #[test]
    fn test_word_before_cursor_at_space() {
        let content = "let x = foo";
        // cursor right after the space between "=" and "foo"
        let word = word_before_cursor(content, 0, 8);
        assert_eq!(word, "");
    }

    #[test]
    fn test_word_before_cursor_multiline() {
        let content = "fn a() {}\nfn handle_";
        // line 1, col 10 (after "handle_")
        let word = word_before_cursor(content, 1, 10);
        assert_eq!(word, "handle_");
    }

    #[test]
    fn test_word_before_cursor_out_of_bounds() {
        let content = "fn a() {}";
        let word = word_before_cursor(content, 99, 0);
        assert_eq!(word, "");
    }

    #[test]
    fn test_complete_kind_preserved() {
        let symbols = vec![
            sym("MyStruct", SymbolKind::Struct),
            sym("my_func", SymbolKind::Function),
        ];
        let items = complete(&[symbols.as_slice()], "");
        let struct_item = items.iter().find(|i| i.label == "MyStruct").expect("struct");
        assert_eq!(struct_item.kind, "struct");
        let func_item = items.iter().find(|i| i.label == "my_func").expect("func");
        assert_eq!(func_item.kind, "function");
    }
}
