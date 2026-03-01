//! Tree-sitter parsing module for Orchestra RAG engine.
//!
//! Provides language detection, grammar registration, and code parsing
//! using Tree-sitter grammars for 14+ languages. Symbols are extracted
//! from parsed ASTs for indexing and navigation.

pub mod registry;
pub mod symbols;
pub mod wrapper;

pub use registry::LanguageRegistry;
pub use symbols::{CodeSymbol, SymbolExtractor, SymbolKind, TextRange};
pub use wrapper::ParserWrapper;

/// Errors that can occur during parsing operations.
#[derive(Debug, thiserror::Error)]
pub enum ParserError {
    #[error("unsupported language: {0}")]
    UnsupportedLanguage(String),

    #[error("failed to parse content: {0}")]
    ParseFailed(String),

    #[error("tree-sitter initialization error: {0}")]
    TreeSitterInit(String),
}

/// Result alias for parser operations.
pub type ParserResult<T> = Result<T, ParserError>;
