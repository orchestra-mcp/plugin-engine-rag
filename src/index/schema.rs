//! Tantivy schema definition for code indexing.
//!
//! Defines the document schema with fields for file path, content,
//! language, symbols, and metadata.

use tantivy::schema::{Field, Schema, STORED, STRING, TEXT};

/// Index schema containing all field definitions.
///
/// This schema is optimized for code search with:
/// - Full-text search on content and symbols
/// - Fast filtering on file paths and languages
/// - JSON metadata for extensibility
#[derive(Debug, Clone)]
pub struct IndexSchema {
    schema: Schema,
    path: Field,
    content: Field,
    language: Field,
    symbols: Field,
    metadata: Field,
}

impl IndexSchema {
    /// Build a new schema with the default code-search fields.
    pub fn new() -> Self {
        let mut builder = Schema::builder();

        // path: stored STRING field for exact file path matching
        let path = builder.add_text_field("path", STRING | STORED);

        // content: full-text indexed field for code search
        let content = builder.add_text_field("content", TEXT | STORED);

        // language: STRING field for language filtering
        let language = builder.add_text_field("language", STRING | STORED);

        // symbols: TEXT field for symbol names (space-separated, indexed)
        let symbols = builder.add_text_field("symbols", TEXT | STORED);

        // metadata: stored JSON text (not indexed for search, only stored)
        let metadata = builder.add_text_field("metadata", STORED);

        let schema = builder.build();

        Self {
            schema,
            path,
            content,
            language,
            symbols,
            metadata,
        }
    }

    /// Returns the underlying Tantivy schema.
    pub fn tantivy_schema(&self) -> &Schema {
        &self.schema
    }

    /// Returns the path field.
    pub fn path(&self) -> Field {
        self.path
    }

    /// Returns the content field.
    pub fn content(&self) -> Field {
        self.content
    }

    /// Returns the language field.
    pub fn language(&self) -> Field {
        self.language
    }

    /// Returns the symbols field.
    pub fn symbols(&self) -> Field {
        self.symbols
    }

    /// Returns the metadata field.
    pub fn metadata(&self) -> Field {
        self.metadata
    }
}

impl Default for IndexSchema {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_has_all_fields() {
        let schema = IndexSchema::new();
        let tantivy_schema = schema.tantivy_schema();

        let path_entry = tantivy_schema.get_field_entry(schema.path());
        assert_eq!(path_entry.name(), "path");

        let content_entry = tantivy_schema.get_field_entry(schema.content());
        assert_eq!(content_entry.name(), "content");

        let language_entry = tantivy_schema.get_field_entry(schema.language());
        assert_eq!(language_entry.name(), "language");

        let symbols_entry = tantivy_schema.get_field_entry(schema.symbols());
        assert_eq!(symbols_entry.name(), "symbols");

        let metadata_entry = tantivy_schema.get_field_entry(schema.metadata());
        assert_eq!(metadata_entry.name(), "metadata");
    }

    #[test]
    fn default_schema_matches_new() {
        let s1 = IndexSchema::new();
        let s2 = IndexSchema::default();
        // Both should have identical field IDs.
        assert_eq!(s1.path().field_id(), s2.path().field_id());
        assert_eq!(s1.content().field_id(), s2.content().field_id());
        assert_eq!(s1.language().field_id(), s2.language().field_id());
    }
}
