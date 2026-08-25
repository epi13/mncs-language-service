//! Static TextMate grammar package for the MNCS language.
//!
//! This crate owns the *presentation* adapter that lets TextMate-compatible
//! environments (GitHub/Linguist, VS Code, Zed, Sublime, …) highlight
//! `.mncs` sources without running the MNCS compiler. It contains no MNCS
//! semantics: syntax and semantics remain owned by `mncs-language`, and this
//! grammar is deliberately shallow.
//!
//! Three responsibilities:
//!
//! 1. [`grammar`] loads and structurally validates
//!    `integration/static-syntax/mncs.tmLanguage.json`;
//! 2. [`tokenizer`] provides a small, honest TextMate-subset tokenization
//!    engine used by tests (the grammar is authored to stay inside the
//!    portable subset that real engines agree on);
//! 3. [`scopes`] keeps the exhaustive authoritative-token → scope mapping
//!    and keyword spelling manifest used for drift conformance against the
//!    live authoritative lexer.

pub mod grammar;
pub mod scopes;
pub mod tokenizer;

/// Canonical TextMate scope for MNCS sources.
pub const SOURCE_SCOPE: &str = "source.mncs";

/// Repository-root-relative path of the grammar asset.
pub const GRAMMAR_PATH: &str = "integration/static-syntax/mncs.tmLanguage.json";

/// The canonical `.mncs` extension (without dot).
pub const CANONICAL_EXTENSION: &str = "mncs";

/// Load and validate the bundled grammar.
///
/// # Errors
/// Returns [`grammar::GrammarError`] when the embedded grammar is malformed,
/// uses unsupported constructs, or fails regex compilation.
pub fn load_grammar() -> Result<grammar::Grammar, grammar::GrammarError> {
    let raw = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../integration/static-syntax/mncs.tmLanguage.json"
    ));
    grammar::Grammar::from_json(raw)
}

/// Tokenize a full document into per-line tokens.
///
/// # Errors
/// Returns [`grammar::GrammarError`] if tokenization hits an unsupported
/// construct (which validation should already have rejected).
pub fn tokenize_document(
    grammar: &grammar::Grammar,
    text: &str,
) -> Result<Vec<Vec<tokenizer::Token>>, grammar::GrammarError> {
    let mut highlighter = tokenizer::Highlighter::new(grammar)?;
    Ok(highlighter.tokenize(text))
}
