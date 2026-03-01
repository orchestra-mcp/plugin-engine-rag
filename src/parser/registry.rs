//! Language registry mapping names and file extensions to Tree-sitter grammars.

use std::collections::HashMap;
use tree_sitter::Language;
use tracing::debug;

/// Maps language names and file extensions to Tree-sitter Language objects.
///
/// Pre-registers 15 languages: rust, go, javascript, typescript, tsx,
/// python, c, cpp, java, html, css, json, toml, yaml, markdown.
pub struct LanguageRegistry {
    /// language name -> Language
    languages: HashMap<String, Language>,
    /// file extension (without dot) -> language name
    extensions: HashMap<String, String>,
}

impl LanguageRegistry {
    /// Create a new registry with all built-in languages pre-registered.
    pub fn new() -> Self {
        let mut reg = Self {
            languages: HashMap::new(),
            extensions: HashMap::new(),
        };
        reg.register_builtins();
        reg
    }

    /// Get a Tree-sitter Language by name.
    pub fn get_language(&self, name: &str) -> Option<Language> {
        self.languages.get(name).cloned()
    }

    /// Detect language name from a file path's extension.
    pub fn detect_language(&self, file_path: &str) -> Option<String> {
        let ext = file_path.rsplit('.').next()?;
        self.extensions.get(ext).cloned()
    }

    /// Register a custom language with its file extensions.
    ///
    /// Plugins can use this to add languages not in the built-in set.
    pub fn register_language(
        &mut self,
        name: &str,
        language: Language,
        exts: &[&str],
    ) {
        debug!(name, ?exts, "registering language");
        self.languages.insert(name.to_string(), language);
        for ext in exts {
            self.extensions.insert(ext.to_string(), name.to_string());
        }
    }

    /// List all registered language names.
    pub fn supported_languages(&self) -> Vec<String> {
        let mut names: Vec<String> = self.languages.keys().cloned().collect();
        names.sort();
        names
    }

    /// Return the set of all registered file extensions (without leading dot).
    ///
    /// Useful for filtering directory walks to only include files that
    /// have a known Tree-sitter grammar.
    pub fn supported_extensions(&self) -> Vec<String> {
        let mut exts: Vec<String> = self.extensions.keys().cloned().collect();
        exts.sort();
        exts
    }

    // -- private ----------------------------------------------------------

    fn register_builtins(&mut self) {
        self.register_language(
            "rust",
            Language::from(tree_sitter_rust::LANGUAGE),
            &["rs"],
        );
        self.register_language(
            "go",
            Language::from(tree_sitter_go::LANGUAGE),
            &["go"],
        );
        self.register_language(
            "javascript",
            Language::from(tree_sitter_javascript::LANGUAGE),
            &["js", "jsx"],
        );
        self.register_language(
            "typescript",
            Language::from(tree_sitter_typescript::LANGUAGE_TYPESCRIPT),
            &["ts"],
        );
        self.register_language(
            "tsx",
            Language::from(tree_sitter_typescript::LANGUAGE_TSX),
            &["tsx"],
        );
        self.register_language(
            "python",
            Language::from(tree_sitter_python::LANGUAGE),
            &["py"],
        );
        self.register_language(
            "c",
            Language::from(tree_sitter_c::LANGUAGE),
            &["c", "h"],
        );
        self.register_language(
            "cpp",
            Language::from(tree_sitter_cpp::LANGUAGE),
            &["cpp", "hpp", "cc"],
        );
        self.register_language(
            "java",
            Language::from(tree_sitter_java::LANGUAGE),
            &["java"],
        );
        self.register_language(
            "html",
            Language::from(tree_sitter_html::LANGUAGE),
            &["html", "htm"],
        );
        self.register_language(
            "css",
            Language::from(tree_sitter_css::LANGUAGE),
            &["css"],
        );
        self.register_language(
            "json",
            Language::from(tree_sitter_json::LANGUAGE),
            &["json"],
        );
        self.register_language(
            "toml",
            Language::from(tree_sitter_toml_ng::LANGUAGE),
            &["toml"],
        );
        self.register_language(
            "yaml",
            Language::from(tree_sitter_yaml::LANGUAGE),
            &["yaml", "yml"],
        );
        self.register_language(
            "markdown",
            Language::from(tree_sitter_md::LANGUAGE),
            &["md"],
        );
    }
}

impl Default for LanguageRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_language_from_extensions() {
        let reg = LanguageRegistry::new();
        assert_eq!(reg.detect_language("main.rs"), Some("rust".into()));
        assert_eq!(reg.detect_language("main.go"), Some("go".into()));
        assert_eq!(reg.detect_language("app.tsx"), Some("tsx".into()));
        assert_eq!(reg.detect_language("lib.py"), Some("python".into()));
        assert_eq!(reg.detect_language("config.json"), Some("json".into()));
        assert_eq!(reg.detect_language("style.css"), Some("css".into()));
        assert_eq!(reg.detect_language("page.html"), Some("html".into()));
        assert_eq!(reg.detect_language("Cargo.toml"), Some("toml".into()));
        assert_eq!(reg.detect_language("ci.yml"), Some("yaml".into()));
        assert_eq!(reg.detect_language("README.md"), Some("markdown".into()));
    }

    #[test]
    fn detect_language_returns_none_for_unknown() {
        let reg = LanguageRegistry::new();
        assert_eq!(reg.detect_language("data.xyz"), None);
    }

    #[test]
    fn get_language_returns_some_for_registered() {
        let reg = LanguageRegistry::new();
        assert!(reg.get_language("rust").is_some());
        assert!(reg.get_language("go").is_some());
        assert!(reg.get_language("python").is_some());
    }

    #[test]
    fn get_language_returns_none_for_unknown() {
        let reg = LanguageRegistry::new();
        assert!(reg.get_language("brainfuck").is_none());
    }

    #[test]
    fn supported_languages_returns_all() {
        let reg = LanguageRegistry::new();
        let langs = reg.supported_languages();
        assert_eq!(langs.len(), 15);
        assert!(langs.contains(&"rust".to_string()));
        assert!(langs.contains(&"typescript".to_string()));
        assert!(langs.contains(&"tsx".to_string()));
        assert!(langs.contains(&"markdown".to_string()));
    }

    #[test]
    fn supported_extensions_returns_all() {
        let reg = LanguageRegistry::new();
        let exts = reg.supported_extensions();
        // Should include at least the common extensions
        assert!(exts.contains(&"rs".to_string()));
        assert!(exts.contains(&"go".to_string()));
        assert!(exts.contains(&"py".to_string()));
        assert!(exts.contains(&"ts".to_string()));
        assert!(exts.contains(&"tsx".to_string()));
        assert!(exts.contains(&"js".to_string()));
        assert!(exts.contains(&"jsx".to_string()));
        assert!(exts.contains(&"java".to_string()));
        assert!(exts.contains(&"json".to_string()));
        assert!(exts.contains(&"toml".to_string()));
        assert!(exts.contains(&"yml".to_string()));
        assert!(exts.contains(&"yaml".to_string()));
        assert!(exts.contains(&"md".to_string()));
        assert!(exts.contains(&"css".to_string()));
        assert!(exts.contains(&"html".to_string()));
    }

    #[test]
    fn register_custom_language() {
        let mut reg = LanguageRegistry::new();
        let count_before = reg.supported_languages().len();

        // Re-register rust grammar under a custom name for testing.
        let lang = Language::from(tree_sitter_rust::LANGUAGE);
        reg.register_language("custom-rust", lang, &["crs"]);

        assert_eq!(reg.supported_languages().len(), count_before + 1);
        assert!(reg.get_language("custom-rust").is_some());
        assert_eq!(reg.detect_language("foo.crs"), Some("custom-rust".into()));
    }
}
