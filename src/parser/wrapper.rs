//! Parser wrapper with language caching for efficient repeated parsing.

use tree_sitter::{Parser, Tree};
use tracing::debug;

use super::registry::LanguageRegistry;
use super::{ParserError, ParserResult};

/// Wraps a Tree-sitter [`Parser`] with language caching.
///
/// Avoids redundant `set_language` calls when parsing multiple files
/// of the same language in sequence.
pub struct ParserWrapper {
    parser: Parser,
    registry: LanguageRegistry,
    current_language: Option<String>,
}

impl ParserWrapper {
    /// Create a new wrapper with the default language registry.
    pub fn new() -> ParserResult<Self> {
        let parser = Parser::new();
        Ok(Self {
            parser,
            registry: LanguageRegistry::new(),
            current_language: None,
        })
    }

    /// Create a wrapper with a custom language registry.
    pub fn with_registry(registry: LanguageRegistry) -> ParserResult<Self> {
        let parser = Parser::new();
        Ok(Self {
            parser,
            registry,
            current_language: None,
        })
    }

    /// Parse source code in the given language.
    pub fn parse(
        &mut self,
        content: &str,
        language: &str,
    ) -> ParserResult<Tree> {
        self.ensure_language(language)?;
        self.parser
            .parse(content, None)
            .ok_or_else(|| ParserError::ParseFailed(
                format!("tree-sitter returned None for language '{language}'"),
            ))
    }

    /// Incrementally parse source code using a previous tree.
    ///
    /// This is significantly faster when only small edits have been made.
    pub fn parse_incremental(
        &mut self,
        content: &str,
        language: &str,
        old_tree: &Tree,
    ) -> ParserResult<Tree> {
        self.ensure_language(language)?;
        self.parser
            .parse(content, Some(old_tree))
            .ok_or_else(|| ParserError::ParseFailed(
                format!("incremental parse returned None for language '{language}'"),
            ))
    }

    /// Get a reference to the inner language registry.
    pub fn registry(&self) -> &LanguageRegistry {
        &self.registry
    }

    /// Get a mutable reference to the inner language registry.
    pub fn registry_mut(&mut self) -> &mut LanguageRegistry {
        // Reset cache because the registry may change.
        self.current_language = None;
        &mut self.registry
    }

    // -- private ----------------------------------------------------------

    /// Set the parser language, skipping if already set.
    fn ensure_language(&mut self, language: &str) -> ParserResult<()> {
        if self.current_language.as_deref() == Some(language) {
            return Ok(());
        }

        let lang = self
            .registry
            .get_language(language)
            .ok_or_else(|| ParserError::UnsupportedLanguage(language.to_string()))?;

        self.parser
            .set_language(&lang)
            .map_err(|e| ParserError::TreeSitterInit(
                format!("failed to set language '{language}': {e}"),
            ))?;

        debug!(language, "parser language set");
        self.current_language = Some(language.to_string());
        Ok(())
    }
}

impl Default for ParserWrapper {
    fn default() -> Self {
        Self::new().expect("failed to create default ParserWrapper")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rust_code() {
        let mut pw = ParserWrapper::new().expect("wrapper creation failed");
        let tree = pw.parse("fn main() { println!(\"hello\"); }", "rust");
        assert!(tree.is_ok());
        let tree = tree.expect("parse failed");
        let root = tree.root_node();
        assert_eq!(root.kind(), "source_file");
        assert!(root.child_count() > 0);
    }

    #[test]
    fn parse_go_code() {
        let mut pw = ParserWrapper::new().expect("wrapper creation failed");
        let code = "package main\n\nfunc main() {\n\tfmt.Println(\"hello\")\n}";
        let tree = pw.parse(code, "go").expect("parse failed");
        assert_eq!(tree.root_node().kind(), "source_file");
    }

    #[test]
    fn parse_javascript_code() {
        let mut pw = ParserWrapper::new().expect("wrapper creation failed");
        let code = "function greet(name) { return `hello ${name}`; }";
        let tree = pw.parse(code, "javascript").expect("parse failed");
        assert_eq!(tree.root_node().kind(), "program");
    }

    #[test]
    fn parse_python_code() {
        let mut pw = ParserWrapper::new().expect("wrapper creation failed");
        let code = "def greet(name):\n    return f'hello {name}'";
        let tree = pw.parse(code, "python").expect("parse failed");
        assert_eq!(tree.root_node().kind(), "module");
    }

    #[test]
    fn parse_unsupported_language_returns_error() {
        let mut pw = ParserWrapper::new().expect("wrapper creation failed");
        let result = pw.parse("code", "brainfuck");
        assert!(result.is_err());
        match result {
            Err(ParserError::UnsupportedLanguage(lang)) => {
                assert_eq!(lang, "brainfuck");
            }
            other => panic!("expected UnsupportedLanguage, got: {other:?}"),
        }
    }

    #[test]
    fn incremental_parse_with_old_tree() {
        let mut pw = ParserWrapper::new().expect("wrapper creation failed");

        let code_v1 = "fn main() {}";
        let tree_v1 = pw.parse(code_v1, "rust").expect("initial parse failed");

        let code_v2 = "fn main() { let x = 1; }";
        let tree_v2 = pw
            .parse_incremental(code_v2, "rust", &tree_v1)
            .expect("incremental parse failed");

        assert_eq!(tree_v2.root_node().kind(), "source_file");
        assert!(tree_v2.root_node().child_count() > 0);
    }

    #[test]
    fn language_caching_avoids_redundant_set() {
        let mut pw = ParserWrapper::new().expect("wrapper creation failed");

        let _ = pw.parse("fn a() {}", "rust").expect("first parse failed");
        let _ = pw.parse("fn b() {}", "rust").expect("second parse failed");

        let _ = pw
            .parse("def a(): pass", "python")
            .expect("python parse failed");
        let _ = pw.parse("fn c() {}", "rust").expect("back-to-rust failed");
    }

    #[test]
    fn register_custom_language_via_wrapper() {
        let mut pw = ParserWrapper::new().expect("wrapper creation failed");

        let lang = tree_sitter::Language::from(tree_sitter_rust::LANGUAGE);
        pw.registry_mut()
            .register_language("custom-rust", lang, &["crs"]);

        let tree = pw
            .parse("fn custom() {}", "custom-rust")
            .expect("custom language parse failed");
        assert_eq!(tree.root_node().kind(), "source_file");
    }
}
