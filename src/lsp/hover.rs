//! Hover information extraction for the LSP module.
//!
//! Given a document and a cursor position (line, col), find the symbol
//! at that position and return:
//!   - The symbol name and kind.
//!   - The first non-empty comment line immediately preceding the symbol's
//!     start line in the document text (used as documentation).
//!   - The symbol's `detail` string if present (e.g. function signature).

use crate::parser::CodeSymbol;

// ---------------------------------------------------------------------------
// HoverInfo
// ---------------------------------------------------------------------------

/// Hover information for a symbol at a given cursor position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoverInfo {
    pub name: String,
    pub kind: String,
    /// Optional documentation extracted from the preceding comment line.
    pub documentation: Option<String>,
    /// Optional type/signature detail (from `CodeSymbol::detail`).
    pub detail: Option<String>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Find the symbol at position `(line, col)` in `symbols` and return its
/// hover information extracted from the document `content`.
///
/// Returns `None` if no symbol covers the given position.
pub fn hover_at(
    symbols: &[CodeSymbol],
    content: &str,
    line: u32,
    col: u32,
) -> Option<HoverInfo> {
    let sym = find_symbol_at(symbols, line, col)?;

    let documentation = extract_preceding_comment(content, sym.range.start_line);

    Some(HoverInfo {
        name: sym.name.clone(),
        kind: sym.kind.to_string(),
        documentation,
        detail: sym.detail.clone(),
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Find the smallest symbol whose range contains (line, col).
///
/// Checks both top-level symbols and their children, preferring the most
/// specific (smallest) match.
fn find_symbol_at(symbols: &[CodeSymbol], line: u32, col: u32) -> Option<&CodeSymbol> {
    let line = line as usize;
    let col = col as usize;

    let mut best: Option<&CodeSymbol> = None;

    for sym in symbols {
        if symbol_contains(sym, line, col) {
            // Check children first for a more specific match.
            if let Some(child_match) = find_symbol_at(&sym.children, line as u32, col as u32) {
                best = Some(child_match);
            } else {
                // Use this symbol if it's a better (smaller) fit.
                match best {
                    None => best = Some(sym),
                    Some(prev) => {
                        let prev_span = prev.range.end_line.saturating_sub(prev.range.start_line);
                        let sym_span = sym.range.end_line.saturating_sub(sym.range.start_line);
                        if sym_span < prev_span {
                            best = Some(sym);
                        }
                    }
                }
            }
        }
    }

    best
}

/// Returns `true` if the symbol's range covers the given 0-based line and column.
fn symbol_contains(sym: &CodeSymbol, line: usize, _col: usize) -> bool {
    sym.range.start_line <= line && line <= sym.range.end_line
}

/// Extract the first non-empty comment-like line immediately before `start_line`.
///
/// Looks at up to 5 lines above `start_line` for a line that starts with a
/// comment marker (`//`, `#`, `/*`, `*`). Returns the first such line found
/// (stripped of comment markers and whitespace), or `None` if none is found.
fn extract_preceding_comment(content: &str, start_line: usize) -> Option<String> {
    let lines: Vec<&str> = content.lines().collect();

    if start_line == 0 || lines.is_empty() {
        return None;
    }

    // Search up to 5 lines above the symbol start.
    let search_from = start_line.saturating_sub(1);
    let search_to = search_from.saturating_sub(4);

    let mut idx = search_from;
    loop {
        if let Some(&line) = lines.get(idx) {
            let trimmed = line.trim();
            if is_comment_line(trimmed) {
                let stripped = strip_comment_markers(trimmed);
                if !stripped.is_empty() {
                    return Some(stripped);
                }
            } else if !trimmed.is_empty() {
                // Hit a non-empty, non-comment line — stop.
                break;
            }
        }

        if idx == search_to || idx == 0 {
            break;
        }
        idx -= 1;
    }

    None
}

/// Check if a trimmed line looks like a comment.
fn is_comment_line(trimmed: &str) -> bool {
    trimmed.starts_with("//")
        || trimmed.starts_with('#')
        || trimmed.starts_with("/*")
        || trimmed.starts_with('*')
        || trimmed.starts_with("--")
}

/// Strip leading comment markers and whitespace from a comment line.
fn strip_comment_markers(s: &str) -> String {
    let stripped = s
        .trim_start_matches("///")
        .trim_start_matches("//!")
        .trim_start_matches("//")
        .trim_start_matches("/**")
        .trim_start_matches("/*")
        .trim_start_matches('*')
        .trim_start_matches('#')
        .trim_start_matches("--")
        .trim();
    stripped.to_string()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{CodeSymbol, SymbolKind, TextRange};

    fn make_sym(name: &str, kind: SymbolKind, start: usize, end: usize) -> CodeSymbol {
        CodeSymbol {
            name: name.to_string(),
            kind,
            range: TextRange {
                start_line: start,
                start_column: 0,
                end_line: end,
                end_column: 0,
            },
            detail: None,
            children: Vec::new(),
        }
    }

    fn make_sym_with_detail(
        name: &str,
        kind: SymbolKind,
        start: usize,
        end: usize,
        detail: &str,
    ) -> CodeSymbol {
        CodeSymbol {
            name: name.to_string(),
            kind,
            range: TextRange {
                start_line: start,
                start_column: 0,
                end_line: end,
                end_column: 0,
            },
            detail: Some(detail.to_string()),
            children: Vec::new(),
        }
    }

    const RUST_WITH_COMMENTS: &str = "\
fn helper() {}

// Adds two numbers together
fn add(a: i32, b: i32) -> i32 {
    a + b
}

/// A point in 2D space
struct Point {
    x: f64,
    y: f64,
}
";

    #[test]
    fn test_hover_finds_function() {
        let symbols = vec![make_sym("add", SymbolKind::Function, 3, 5)];
        let info = hover_at(&symbols, RUST_WITH_COMMENTS, 3, 0);
        assert!(info.is_some(), "expected hover info at line 3");
        let info = info.expect("missing hover");
        assert_eq!(info.name, "add");
        assert_eq!(info.kind, "function");
    }

    #[test]
    fn test_hover_extracts_preceding_comment() {
        // Line 2 = "// Adds two numbers together", line 3 = "fn add..."
        let symbols = vec![make_sym("add", SymbolKind::Function, 3, 5)];
        let info = hover_at(&symbols, RUST_WITH_COMMENTS, 3, 0);
        assert!(info.is_some());
        let info = info.expect("missing hover");
        assert!(
            info.documentation.is_some(),
            "expected documentation from comment"
        );
        let doc = info.documentation.expect("missing doc");
        assert!(
            doc.contains("Adds"),
            "doc should contain comment text, got: {doc}"
        );
    }

    #[test]
    fn test_hover_extracts_doc_comment() {
        // Line 7 = "/// A point in 2D space", line 8 = "struct Point {"
        let symbols = vec![make_sym("Point", SymbolKind::Struct, 8, 11)];
        let info = hover_at(&symbols, RUST_WITH_COMMENTS, 8, 0);
        assert!(info.is_some());
        let info = info.expect("missing hover");
        assert!(info.documentation.is_some(), "expected doc comment");
        let doc = info.documentation.expect("missing doc");
        assert!(doc.contains("point"), "doc should contain 'point', got: {doc}");
    }

    #[test]
    fn test_hover_no_comment() {
        let symbols = vec![make_sym("helper", SymbolKind::Function, 0, 0)];
        // helper is at line 0, no lines above it.
        let info = hover_at(&symbols, RUST_WITH_COMMENTS, 0, 0);
        assert!(info.is_some());
        let info = info.expect("missing hover");
        assert!(
            info.documentation.is_none(),
            "expected no doc at line 0"
        );
    }

    #[test]
    fn test_hover_includes_detail() {
        let symbols = vec![make_sym_with_detail(
            "calc",
            SymbolKind::Function,
            0,
            3,
            "(a: i32, b: i32) -> i32",
        )];
        let content = "fn calc(a: i32, b: i32) -> i32 {\n    a + b\n}\n";
        let info = hover_at(&symbols, content, 0, 0);
        assert!(info.is_some());
        let info = info.expect("missing hover");
        assert_eq!(info.detail.as_deref(), Some("(a: i32, b: i32) -> i32"));
    }

    #[test]
    fn test_hover_out_of_range() {
        let symbols = vec![make_sym("foo", SymbolKind::Function, 5, 10)];
        let info = hover_at(&symbols, "fn foo() {}", 0, 0);
        assert!(info.is_none(), "should not match a symbol at line 0");
    }

    #[test]
    fn test_hover_prefers_child_symbol() {
        let method = CodeSymbol {
            name: "inner".to_string(),
            kind: SymbolKind::Method,
            range: TextRange {
                start_line: 2,
                start_column: 4,
                end_line: 4,
                end_column: 5,
            },
            detail: None,
            children: Vec::new(),
        };
        let class = CodeSymbol {
            name: "Outer".to_string(),
            kind: SymbolKind::Class,
            range: TextRange {
                start_line: 0,
                start_column: 0,
                end_line: 6,
                end_column: 1,
            },
            detail: None,
            children: vec![method],
        };

        let content = "class Outer:\n  pass\n  def inner(self):\n    pass\n\n  pass\n";
        let info = hover_at(&[class], content, 2, 4);
        assert!(info.is_some());
        let info = info.expect("missing hover");
        // Should prefer the method child, not the outer class.
        assert_eq!(info.name, "inner");
        assert_eq!(info.kind, "method");
    }

    #[test]
    fn test_strip_comment_markers_rust() {
        assert_eq!(strip_comment_markers("// hello"), "hello");
        assert_eq!(strip_comment_markers("/// doc comment"), "doc comment");
        assert_eq!(strip_comment_markers("//! inner doc"), "inner doc");
        assert_eq!(strip_comment_markers("* block item"), "block item");
        assert_eq!(strip_comment_markers("# python"), "python");
    }

    #[test]
    fn test_python_comment_hover() {
        let content = "# Returns the sum\ndef add(a, b):\n    return a + b\n";
        let symbols = vec![make_sym("add", SymbolKind::Function, 1, 2)];
        let info = hover_at(&symbols, content, 1, 0);
        assert!(info.is_some());
        let info = info.expect("missing hover");
        assert!(
            info.documentation
                .as_deref()
                .map_or(false, |d| d.contains("sum")),
            "expected doc from Python comment, got: {:?}",
            info.documentation
        );
    }
}
