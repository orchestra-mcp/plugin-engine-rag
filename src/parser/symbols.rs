//! Symbol extraction from parsed Tree-sitter syntax trees.
//!
//! Walks a Tree-sitter CST and extracts language-aware symbols such as
//! functions, classes, structs, imports, etc. Each symbol carries its name,
//! kind, source range, optional detail (e.g. parameter list), and nested
//! children (e.g. methods inside a class).

use serde::{Deserialize, Serialize};
use tracing::debug;
use tree_sitter::{Node, Tree};

use super::{ParserResult, ParserWrapper};

// ---------------------------------------------------------------------------
// Core types
// ---------------------------------------------------------------------------

/// The kind of a code symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Function,
    Method,
    Class,
    Struct,
    Interface,
    Enum,
    Trait,
    Variable,
    Constant,
    Module,
    Import,
    Export,
    Field,
    Type,
    Property,
    Constructor,
    Event,
}

impl std::fmt::Display for SymbolKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            Self::Function => "function",
            Self::Method => "method",
            Self::Class => "class",
            Self::Struct => "struct",
            Self::Interface => "interface",
            Self::Enum => "enum",
            Self::Trait => "trait",
            Self::Variable => "variable",
            Self::Constant => "constant",
            Self::Module => "module",
            Self::Import => "import",
            Self::Export => "export",
            Self::Field => "field",
            Self::Type => "type",
            Self::Property => "property",
            Self::Constructor => "constructor",
            Self::Event => "event",
        };
        f.write_str(label)
    }
}

impl SymbolKind {
    /// Parse a SymbolKind from a string name.
    pub fn from_str_name(s: &str) -> Option<Self> {
        match s {
            "function" => Some(Self::Function),
            "method" => Some(Self::Method),
            "class" => Some(Self::Class),
            "struct" => Some(Self::Struct),
            "interface" => Some(Self::Interface),
            "enum" => Some(Self::Enum),
            "trait" => Some(Self::Trait),
            "variable" => Some(Self::Variable),
            "constant" => Some(Self::Constant),
            "module" => Some(Self::Module),
            "import" => Some(Self::Import),
            "export" => Some(Self::Export),
            "field" => Some(Self::Field),
            "type" => Some(Self::Type),
            "property" => Some(Self::Property),
            "constructor" => Some(Self::Constructor),
            "event" => Some(Self::Event),
            _ => None,
        }
    }
}

/// A zero-based line/column range in source code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextRange {
    pub start_line: usize,
    pub start_column: usize,
    pub end_line: usize,
    pub end_column: usize,
}

impl From<tree_sitter::Range> for TextRange {
    fn from(r: tree_sitter::Range) -> Self {
        Self {
            start_line: r.start_point.row,
            start_column: r.start_point.column,
            end_line: r.end_point.row,
            end_column: r.end_point.column,
        }
    }
}

/// A symbol extracted from source code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeSymbol {
    pub name: String,
    pub kind: SymbolKind,
    pub range: TextRange,
    /// Optional detail such as a function signature or type annotation.
    pub detail: Option<String>,
    /// Nested child symbols (e.g. methods inside a class).
    pub children: Vec<CodeSymbol>,
}

// ---------------------------------------------------------------------------
// SymbolExtractor
// ---------------------------------------------------------------------------

/// Extracts code symbols from source text using Tree-sitter.
pub struct SymbolExtractor {
    parser: ParserWrapper,
}

impl SymbolExtractor {
    /// Create a new extractor backed by the default [`ParserWrapper`].
    pub fn new() -> ParserResult<Self> {
        Ok(Self {
            parser: ParserWrapper::new()?,
        })
    }

    /// Parse `content` in the given `language` and return top-level symbols.
    pub fn extract_symbols(
        &mut self,
        content: &str,
        language: &str,
    ) -> ParserResult<Vec<CodeSymbol>> {
        let tree = self.parser.parse(content, language)?;
        let source = content.as_bytes();

        let lang_category = categorize_language(language);
        debug!(language, ?lang_category, "extracting symbols");

        let symbols = collect_symbols(&tree, source, lang_category);
        debug!(count = symbols.len(), "symbols extracted");
        Ok(symbols)
    }
}

// ---------------------------------------------------------------------------
// Language categories
// ---------------------------------------------------------------------------

/// Broad language category that determines which node-type mapping to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LangCategory {
    Rust,
    Go,
    JavaScript,
    TypeScript,
    Python,
    Java,
    Generic,
}

fn categorize_language(lang: &str) -> LangCategory {
    match lang {
        "rust" => LangCategory::Rust,
        "go" => LangCategory::Go,
        "javascript" => LangCategory::JavaScript,
        "typescript" | "tsx" => LangCategory::TypeScript,
        "python" => LangCategory::Python,
        "java" => LangCategory::Java,
        _ => LangCategory::Generic,
    }
}

// ---------------------------------------------------------------------------
// Node-type to SymbolKind mapping
// ---------------------------------------------------------------------------

/// Try to map a Tree-sitter node type to a `SymbolKind` for the given
/// language. Returns `None` when the node type is not interesting.
fn node_kind_to_symbol(node_type: &str, lang: LangCategory) -> Option<SymbolKind> {
    match lang {
        LangCategory::Rust => match node_type {
            "function_item" => Some(SymbolKind::Function),
            "struct_item" => Some(SymbolKind::Struct),
            "enum_item" => Some(SymbolKind::Enum),
            "trait_item" => Some(SymbolKind::Trait),
            "impl_item" => Some(SymbolKind::Class),
            "const_item" => Some(SymbolKind::Constant),
            "static_item" => Some(SymbolKind::Constant),
            "type_item" => Some(SymbolKind::Type),
            "use_declaration" => Some(SymbolKind::Import),
            "mod_item" => Some(SymbolKind::Module),
            _ => None,
        },
        LangCategory::Go => match node_type {
            "function_declaration" => Some(SymbolKind::Function),
            "method_declaration" => Some(SymbolKind::Method),
            "type_declaration" => Some(SymbolKind::Type),
            "import_declaration" => Some(SymbolKind::Import),
            "const_declaration" => Some(SymbolKind::Constant),
            "var_declaration" => Some(SymbolKind::Variable),
            "package_clause" => Some(SymbolKind::Module),
            _ => None,
        },
        LangCategory::JavaScript => match node_type {
            "function_declaration" => Some(SymbolKind::Function),
            "class_declaration" => Some(SymbolKind::Class),
            "method_definition" => Some(SymbolKind::Method),
            "variable_declaration" | "lexical_declaration" => Some(SymbolKind::Variable),
            "import_statement" => Some(SymbolKind::Import),
            "export_statement" => Some(SymbolKind::Export),
            _ => None,
        },
        LangCategory::TypeScript => match node_type {
            "function_declaration" => Some(SymbolKind::Function),
            "class_declaration" => Some(SymbolKind::Class),
            "method_definition" => Some(SymbolKind::Method),
            "variable_declaration" | "lexical_declaration" => Some(SymbolKind::Variable),
            "import_statement" => Some(SymbolKind::Import),
            "export_statement" => Some(SymbolKind::Export),
            "interface_declaration" => Some(SymbolKind::Interface),
            "type_alias_declaration" => Some(SymbolKind::Type),
            "enum_declaration" => Some(SymbolKind::Enum),
            _ => None,
        },
        LangCategory::Python => match node_type {
            "function_definition" => Some(SymbolKind::Function),
            "class_definition" => Some(SymbolKind::Class),
            "import_statement" | "import_from_statement" => Some(SymbolKind::Import),
            _ => None,
        },
        LangCategory::Java => match node_type {
            "class_declaration" => Some(SymbolKind::Class),
            "interface_declaration" => Some(SymbolKind::Interface),
            "method_declaration" => Some(SymbolKind::Method),
            "field_declaration" => Some(SymbolKind::Field),
            "enum_declaration" => Some(SymbolKind::Enum),
            "constructor_declaration" => Some(SymbolKind::Constructor),
            "import_declaration" => Some(SymbolKind::Import),
            _ => None,
        },
        LangCategory::Generic => generic_node_kind(node_type),
    }
}

/// Heuristic mapping for languages without dedicated rules.
fn generic_node_kind(node_type: &str) -> Option<SymbolKind> {
    let lower = node_type.to_ascii_lowercase();
    if lower.contains("function") || lower.contains("func_def") {
        Some(SymbolKind::Function)
    } else if lower.contains("class") {
        Some(SymbolKind::Class)
    } else if lower.contains("struct") {
        Some(SymbolKind::Struct)
    } else if lower.contains("import") {
        Some(SymbolKind::Import)
    } else if lower.contains("method") {
        Some(SymbolKind::Method)
    } else if lower.contains("type") && lower.contains("declaration") {
        Some(SymbolKind::Type)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Tree walking helpers
// ---------------------------------------------------------------------------

/// Walk top-level children of the tree and collect symbols.
fn collect_symbols(tree: &Tree, source: &[u8], lang: LangCategory) -> Vec<CodeSymbol> {
    let root = tree.root_node();
    let mut symbols = Vec::new();
    let mut cursor = root.walk();

    for child in root.children(&mut cursor) {
        collect_node_symbols(child, source, lang, &mut symbols);
    }

    symbols
}

/// Recursively inspect `node` and, if it matches a symbol kind for the
/// language, build a `CodeSymbol` (with nested children where appropriate).
/// Otherwise recurse into children looking for deeper symbols.
fn collect_node_symbols(
    node: Node<'_>,
    source: &[u8],
    lang: LangCategory,
    out: &mut Vec<CodeSymbol>,
) {
    let node_type = node.kind();

    if let Some(kind) = node_kind_to_symbol(node_type, lang) {
        if let Some(sym) = build_symbol(node, source, kind, lang) {
            out.push(sym);
            return;
        }
    }

    // For export statements we need to dig into the child declaration.
    if node_type == "export_statement" {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            collect_node_symbols(child, source, lang, out);
        }
        return;
    }

    // For Python top-level assignments, treat as Variable.
    if lang == LangCategory::Python
        && node_type == "expression_statement"
        && node.parent().map_or(false, |p| p.kind() == "module")
    {
        if let Some(assign) = find_child_by_kind(node, "assignment") {
            if let Some(name) = extract_name_from_node(assign, source) {
                out.push(CodeSymbol {
                    name,
                    kind: SymbolKind::Variable,
                    range: TextRange::from(node.range()),
                    detail: None,
                    children: Vec::new(),
                });
                return;
            }
        }
    }

    // Not a symbol node -- descend into children.
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_node_symbols(child, source, lang, out);
    }
}

/// Build a [`CodeSymbol`] from a matched Tree-sitter node.
fn build_symbol(
    node: Node<'_>,
    source: &[u8],
    kind: SymbolKind,
    lang: LangCategory,
) -> Option<CodeSymbol> {
    let name = extract_name(node, source, kind, lang)?;
    let detail = extract_detail(node, source, kind);
    let children = extract_children(node, source, kind, lang);
    let range = TextRange::from(node.range());

    Some(CodeSymbol {
        name,
        kind,
        range,
        detail,
        children,
    })
}

// ---------------------------------------------------------------------------
// Name extraction
// ---------------------------------------------------------------------------

/// Extract the symbol name from a node, trying several strategies in order.
fn extract_name(
    node: Node<'_>,
    source: &[u8],
    kind: SymbolKind,
    lang: LangCategory,
) -> Option<String> {
    // For imports, use the full node text (trimmed).
    if kind == SymbolKind::Import || kind == SymbolKind::Export {
        return node_text(node, source).map(truncate_import);
    }

    // For Go type_declaration, dig into type_spec child.
    if lang == LangCategory::Go && node.kind() == "type_declaration" {
        if let Some(spec) = find_child_by_kind(node, "type_spec") {
            return extract_name_from_node(spec, source);
        }
    }

    // For Rust impl blocks, look for the type being implemented.
    if lang == LangCategory::Rust && node.kind() == "impl_item" {
        return extract_rust_impl_name(node, source);
    }

    extract_name_from_node(node, source)
}

/// Standard name extraction: try field `name`, then `identifier` children.
fn extract_name_from_node(node: Node<'_>, source: &[u8]) -> Option<String> {
    // 1. Try child_by_field_name("name").
    if let Some(name_node) = node.child_by_field_name("name") {
        return node_text(name_node, source);
    }
    // 2. First `identifier` child.
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "identifier"
            || child.kind() == "type_identifier"
            || child.kind() == "package_identifier"
        {
            return node_text(child, source);
        }
    }
    None
}

/// For Rust `impl_item`, extract `"impl Foo"` or `"impl Trait for Foo"`.
fn extract_rust_impl_name(node: Node<'_>, source: &[u8]) -> Option<String> {
    let mut type_names: Vec<String> = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "type_identifier" || child.kind() == "scoped_type_identifier" {
            if let Some(t) = node_text(child, source) {
                type_names.push(t);
            }
        }
        // Also capture generic_type (e.g. `Vec<T>`).
        if child.kind() == "generic_type" {
            if let Some(t) = node_text(child, source) {
                type_names.push(t);
            }
        }
    }
    match type_names.len() {
        0 => None,
        1 => Some(format!("impl {}", type_names[0])),
        _ => {
            // trait impl: `impl Trait for Type`
            let trait_name = &type_names[0];
            let type_name = &type_names[1];
            Some(format!("impl {trait_name} for {type_name}"))
        }
    }
}

// ---------------------------------------------------------------------------
// Detail extraction
// ---------------------------------------------------------------------------

/// Extract an optional detail string (e.g. parameter list + return type).
fn extract_detail(node: Node<'_>, source: &[u8], kind: SymbolKind) -> Option<String> {
    match kind {
        SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor => {
            extract_function_detail(node, source)
        }
        _ => None,
    }
}

/// Build a function/method detail from its parameters and return type nodes.
fn extract_function_detail(node: Node<'_>, source: &[u8]) -> Option<String> {
    let params = node
        .child_by_field_name("parameters")
        .or_else(|| find_child_by_kind(node, "formal_parameters"))
        .or_else(|| find_child_by_kind(node, "parameter_list"))
        .or_else(|| find_child_by_kind(node, "parameters"));

    let params_text = params.and_then(|n| node_text(n, source));

    let return_type = node
        .child_by_field_name("return_type")
        .or_else(|| find_child_by_kind(node, "return_type"))
        .or_else(|| node.child_by_field_name("result"));

    let return_text = return_type.and_then(|n| node_text(n, source));

    match (params_text, return_text) {
        (Some(p), Some(r)) => Some(format!("{p} -> {r}")),
        (Some(p), None) => Some(p),
        (None, Some(r)) => Some(format!("-> {r}")),
        (None, None) => None,
    }
}

// ---------------------------------------------------------------------------
// Child symbol extraction (hierarchical)
// ---------------------------------------------------------------------------

/// For container symbols (class, struct, impl, trait), extract nested symbols
/// as children.
fn extract_children(
    node: Node<'_>,
    source: &[u8],
    kind: SymbolKind,
    lang: LangCategory,
) -> Vec<CodeSymbol> {
    match kind {
        SymbolKind::Class | SymbolKind::Struct | SymbolKind::Trait | SymbolKind::Interface => {
            extract_nested_symbols(node, source, lang)
        }
        _ => Vec::new(),
    }
}

/// Walk all descendants of a container node and extract symbols that live
/// one level below (methods, properties, constructors).
fn extract_nested_symbols(
    parent: Node<'_>,
    source: &[u8],
    lang: LangCategory,
) -> Vec<CodeSymbol> {
    let mut children = Vec::new();

    // Find the body node that contains children.
    let body = find_body_node(parent);
    let walk_root = body.unwrap_or(parent);

    let mut cursor = walk_root.walk();
    for child in walk_root.children(&mut cursor) {
        if let Some(kind) = classify_nested_node(child.kind(), lang) {
            if let Some(sym) = build_symbol(child, source, kind, lang) {
                children.push(sym);
            }
        }
    }

    children
}

/// Classify a node inside a class/struct/impl body as a nested symbol kind.
fn classify_nested_node(node_type: &str, lang: LangCategory) -> Option<SymbolKind> {
    match lang {
        LangCategory::Rust => match node_type {
            "function_item" | "function_signature_item" => Some(SymbolKind::Method),
            "const_item" => Some(SymbolKind::Constant),
            "type_item" => Some(SymbolKind::Type),
            _ => None,
        },
        LangCategory::Go => match node_type {
            "method_declaration" => Some(SymbolKind::Method),
            _ => None,
        },
        LangCategory::JavaScript | LangCategory::TypeScript => match node_type {
            "method_definition" => Some(SymbolKind::Method),
            "public_field_definition" | "field_definition" => Some(SymbolKind::Property),
            _ => None,
        },
        LangCategory::Python => match node_type {
            "function_definition" => Some(SymbolKind::Method),
            _ => None,
        },
        LangCategory::Java => match node_type {
            "method_declaration" => Some(SymbolKind::Method),
            "constructor_declaration" => Some(SymbolKind::Constructor),
            "field_declaration" => Some(SymbolKind::Field),
            _ => None,
        },
        LangCategory::Generic => {
            if node_type.contains("method") || node_type.contains("function") {
                Some(SymbolKind::Method)
            } else {
                None
            }
        }
    }
}

/// Attempt to find the body/declaration_list node of a container.
fn find_body_node(node: Node<'_>) -> Option<Node<'_>> {
    node.child_by_field_name("body")
        .or_else(|| find_child_by_kind(node, "declaration_list"))
        .or_else(|| find_child_by_kind(node, "class_body"))
        .or_else(|| find_child_by_kind(node, "block"))
}

// ---------------------------------------------------------------------------
// Utility helpers
// ---------------------------------------------------------------------------

/// Get the UTF-8 text of a node from the source bytes.
fn node_text(node: Node<'_>, source: &[u8]) -> Option<String> {
    node.utf8_text(source).ok().map(|s| s.to_string())
}

/// Find the first direct child with the given `kind`.
fn find_child_by_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    let mut cursor = node.walk();
    let result = node.children(&mut cursor)
        .find(|child| child.kind() == kind);
    result
}

/// Truncate an import/export statement to a reasonable length for the name.
fn truncate_import(text: String) -> String {
    // Strip trailing semicolons and whitespace.
    let trimmed = text.trim().trim_end_matches(';').trim();
    if trimmed.len() > 120 {
        format!("{}...", &trimmed[..117])
    } else {
        trimmed.to_string()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::ParserError;

    /// Helper: create an extractor and extract symbols.
    fn extract(code: &str, lang: &str) -> Vec<CodeSymbol> {
        let mut ext = SymbolExtractor::new().expect("extractor creation failed");
        ext.extract_symbols(code, lang).expect("extraction failed")
    }

    // -- Rust ---------------------------------------------------------------

    #[test]
    fn rust_functions() {
        let code = r#"
fn add(a: i32, b: i32) -> i32 {
    a + b
}

fn greet(name: &str) {
    println!("hello {}", name);
}
"#;
        let syms = extract(code, "rust");
        assert_eq!(syms.len(), 2);

        assert_eq!(syms[0].name, "add");
        assert_eq!(syms[0].kind, SymbolKind::Function);
        assert!(syms[0].detail.is_some());
        let detail = syms[0].detail.as_ref().expect("detail missing");
        assert!(detail.contains("a: i32"), "detail was: {detail}");

        assert_eq!(syms[1].name, "greet");
        assert_eq!(syms[1].kind, SymbolKind::Function);
    }

    #[test]
    fn rust_structs_and_enums() {
        let code = r#"
struct Point {
    x: f64,
    y: f64,
}

enum Color {
    Red,
    Green,
    Blue,
}
"#;
        let syms = extract(code, "rust");
        assert_eq!(syms.len(), 2);

        assert_eq!(syms[0].name, "Point");
        assert_eq!(syms[0].kind, SymbolKind::Struct);

        assert_eq!(syms[1].name, "Color");
        assert_eq!(syms[1].kind, SymbolKind::Enum);
    }

    #[test]
    fn rust_impl_with_methods() {
        let code = r#"
struct Calc;

impl Calc {
    fn new() -> Self {
        Calc
    }

    fn add(&self, a: i32, b: i32) -> i32 {
        a + b
    }
}
"#;
        let syms = extract(code, "rust");
        assert!(syms.len() >= 2, "expected at least 2 symbols, got {}", syms.len());

        let impl_sym = syms.iter().find(|s| s.kind == SymbolKind::Class).expect("no impl symbol");
        assert!(
            impl_sym.name.contains("Calc"),
            "impl name should contain Calc, got: {}",
            impl_sym.name
        );
        assert!(
            !impl_sym.children.is_empty(),
            "impl block should have children"
        );
        assert!(impl_sym.children.iter().any(|c| c.name == "new"));
        assert!(impl_sym.children.iter().any(|c| c.name == "add"));
        for child in &impl_sym.children {
            assert_eq!(child.kind, SymbolKind::Method);
        }
    }

    #[test]
    fn rust_use_imports() {
        let code = r#"
use std::collections::HashMap;
use std::io::{self, Read};
"#;
        let syms = extract(code, "rust");
        assert_eq!(syms.len(), 2);
        for sym in &syms {
            assert_eq!(sym.kind, SymbolKind::Import);
            assert!(sym.name.starts_with("use "), "name was: {}", sym.name);
        }
    }

    #[test]
    fn rust_trait() {
        let code = r#"
trait Greet {
    fn hello(&self);
}
"#;
        let syms = extract(code, "rust");
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name, "Greet");
        assert_eq!(syms[0].kind, SymbolKind::Trait);
        assert_eq!(syms[0].children.len(), 1);
        assert_eq!(syms[0].children[0].name, "hello");
        assert_eq!(syms[0].children[0].kind, SymbolKind::Method);
    }

    #[test]
    fn rust_module_and_const() {
        let code = r#"
mod utils;

const MAX: usize = 100;
"#;
        let syms = extract(code, "rust");
        assert_eq!(syms.len(), 2);

        let module = syms.iter().find(|s| s.kind == SymbolKind::Module).expect("no module");
        assert_eq!(module.name, "utils");

        let constant = syms.iter().find(|s| s.kind == SymbolKind::Constant).expect("no const");
        assert_eq!(constant.name, "MAX");
    }

    // -- Go -----------------------------------------------------------------

    #[test]
    fn go_functions() {
        let code = r#"
package main

func Add(a int, b int) int {
    return a + b
}

func Greet(name string) {
    fmt.Println("hello", name)
}
"#;
        let syms = extract(code, "go");
        let funcs: Vec<&CodeSymbol> = syms.iter().filter(|s| s.kind == SymbolKind::Function).collect();
        assert_eq!(funcs.len(), 2);
        assert_eq!(funcs[0].name, "Add");
        assert_eq!(funcs[1].name, "Greet");

        let pkg = syms.iter().find(|s| s.kind == SymbolKind::Module);
        assert!(pkg.is_some(), "should detect package clause");
        assert_eq!(pkg.expect("missing pkg").name, "main");
    }

    #[test]
    fn go_type_declarations() {
        let code = r#"
package main

type Point struct {
    X float64
    Y float64
}

type Handler func(w http.ResponseWriter, r *http.Request)
"#;
        let syms = extract(code, "go");
        let types: Vec<&CodeSymbol> = syms.iter().filter(|s| s.kind == SymbolKind::Type).collect();
        assert!(types.len() >= 2, "expected at least 2 type decls, got {}", types.len());
    }

    #[test]
    fn go_imports() {
        let code = r#"
package main

import (
    "fmt"
    "net/http"
)
"#;
        let syms = extract(code, "go");
        let imports: Vec<&CodeSymbol> = syms.iter().filter(|s| s.kind == SymbolKind::Import).collect();
        assert!(!imports.is_empty(), "should detect go imports");
    }

    // -- Python -------------------------------------------------------------

    #[test]
    fn python_classes_and_methods() {
        let code = r#"
class Animal:
    def __init__(self, name):
        self.name = name

    def speak(self):
        return "..."
"#;
        let syms = extract(code, "python");
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name, "Animal");
        assert_eq!(syms[0].kind, SymbolKind::Class);
        assert_eq!(syms[0].children.len(), 2);
        assert_eq!(syms[0].children[0].name, "__init__");
        assert_eq!(syms[0].children[0].kind, SymbolKind::Method);
        assert_eq!(syms[0].children[1].name, "speak");
    }

    #[test]
    fn python_functions() {
        let code = r#"
def add(a, b):
    return a + b

def multiply(a, b):
    return a * b
"#;
        let syms = extract(code, "python");
        assert_eq!(syms.len(), 2);
        assert_eq!(syms[0].name, "add");
        assert_eq!(syms[0].kind, SymbolKind::Function);
        assert_eq!(syms[1].name, "multiply");
    }

    #[test]
    fn python_imports() {
        let code = r#"
import os
from pathlib import Path
from typing import List, Optional
"#;
        let syms = extract(code, "python");
        let imports: Vec<&CodeSymbol> = syms.iter().filter(|s| s.kind == SymbolKind::Import).collect();
        assert_eq!(imports.len(), 3, "expected 3 imports, got: {:?}", imports);
    }

    // -- JavaScript ---------------------------------------------------------

    #[test]
    fn javascript_functions_and_classes() {
        let code = r#"
function greet(name) {
    return "hello " + name;
}

class Calculator {
    add(a, b) {
        return a + b;
    }

    subtract(a, b) {
        return a - b;
    }
}
"#;
        let syms = extract(code, "javascript");
        assert!(syms.len() >= 2, "expected at least 2 symbols, got {}", syms.len());

        let func = syms.iter().find(|s| s.kind == SymbolKind::Function).expect("no function");
        assert_eq!(func.name, "greet");

        let class = syms.iter().find(|s| s.kind == SymbolKind::Class).expect("no class");
        assert_eq!(class.name, "Calculator");
        assert_eq!(class.children.len(), 2);
        assert_eq!(class.children[0].name, "add");
        assert_eq!(class.children[0].kind, SymbolKind::Method);
        assert_eq!(class.children[1].name, "subtract");
    }

    #[test]
    fn javascript_imports() {
        let code = r#"
import React from 'react';
import { useState, useEffect } from 'react';
"#;
        let syms = extract(code, "javascript");
        let imports: Vec<&CodeSymbol> = syms.iter().filter(|s| s.kind == SymbolKind::Import).collect();
        assert_eq!(imports.len(), 2);
    }

    // -- TypeScript ---------------------------------------------------------

    #[test]
    fn typescript_interfaces() {
        let code = r#"
interface User {
    id: number;
    name: string;
    email: string;
}

interface Serializable {
    serialize(): string;
}
"#;
        let syms = extract(code, "typescript");
        let ifaces: Vec<&CodeSymbol> = syms
            .iter()
            .filter(|s| s.kind == SymbolKind::Interface)
            .collect();
        assert_eq!(ifaces.len(), 2);
        assert_eq!(ifaces[0].name, "User");
        assert_eq!(ifaces[1].name, "Serializable");
    }

    #[test]
    fn typescript_type_alias() {
        let code = r#"
type ID = string | number;
type Handler = (req: Request) => Response;
"#;
        let syms = extract(code, "typescript");
        let types: Vec<&CodeSymbol> = syms.iter().filter(|s| s.kind == SymbolKind::Type).collect();
        assert_eq!(types.len(), 2, "expected 2 type aliases, got: {:?}", types);
    }

    #[test]
    fn typescript_function_and_class() {
        let code = r#"
function fetchUser(id: number): Promise<User> {
    return api.get(`/users/${id}`);
}

class UserService {
    getAll() {
        return [];
    }
}
"#;
        let syms = extract(code, "typescript");
        let func = syms.iter().find(|s| s.kind == SymbolKind::Function);
        assert!(func.is_some());
        assert_eq!(func.expect("missing func").name, "fetchUser");

        let class = syms.iter().find(|s| s.kind == SymbolKind::Class);
        assert!(class.is_some());
        let class = class.expect("missing class");
        assert_eq!(class.name, "UserService");
        assert_eq!(class.children.len(), 1);
    }

    // -- Edge cases ---------------------------------------------------------

    #[test]
    fn empty_code_returns_empty_symbols() {
        let syms = extract("", "rust");
        assert!(syms.is_empty());
    }

    #[test]
    fn whitespace_only_returns_empty_symbols() {
        let syms = extract("   \n\n  \t  \n", "python");
        assert!(syms.is_empty());
    }

    #[test]
    fn unsupported_language_returns_error() {
        let mut ext = SymbolExtractor::new().expect("extractor creation failed");
        let result = ext.extract_symbols("code", "brainfuck");
        assert!(result.is_err());
        match result {
            Err(ParserError::UnsupportedLanguage(lang)) => {
                assert_eq!(lang, "brainfuck");
            }
            other => panic!("expected UnsupportedLanguage, got: {other:?}"),
        }
    }

    #[test]
    fn hierarchical_python_class() {
        let code = r#"
class Stack:
    def __init__(self):
        self.items = []

    def push(self, item):
        self.items.append(item)

    def pop(self):
        return self.items.pop()

    def is_empty(self):
        return len(self.items) == 0
"#;
        let syms = extract(code, "python");
        assert_eq!(syms.len(), 1);
        let class = &syms[0];
        assert_eq!(class.name, "Stack");
        assert_eq!(class.kind, SymbolKind::Class);
        assert_eq!(class.children.len(), 4);
        let names: Vec<&str> = class.children.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["__init__", "push", "pop", "is_empty"]);
    }

    #[test]
    fn text_range_from_tree_sitter() {
        let ts_range = tree_sitter::Range {
            start_byte: 0,
            end_byte: 10,
            start_point: tree_sitter::Point { row: 1, column: 0 },
            end_point: tree_sitter::Point { row: 1, column: 10 },
        };
        let tr = TextRange::from(ts_range);
        assert_eq!(tr.start_line, 1);
        assert_eq!(tr.start_column, 0);
        assert_eq!(tr.end_line, 1);
        assert_eq!(tr.end_column, 10);
    }

    #[test]
    fn symbol_kind_display() {
        assert_eq!(SymbolKind::Function.to_string(), "function");
        assert_eq!(SymbolKind::Class.to_string(), "class");
        assert_eq!(SymbolKind::Interface.to_string(), "interface");
        assert_eq!(SymbolKind::Constructor.to_string(), "constructor");
    }

    #[test]
    fn generic_language_fallback() {
        let code = r#"
void hello() {
    printf("hello\n");
}
"#;
        let syms = extract(code, "c");
        let funcs: Vec<&CodeSymbol> = syms.iter().filter(|s| s.kind == SymbolKind::Function).collect();
        assert!(!funcs.is_empty(), "generic fallback should find C functions");
    }
}
