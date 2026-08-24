//! Authoritative analysis and identity-bound snapshots.
//!
//! One [`DocumentAnalysis`] is an immutable snapshot binding a document's
//! exact source state (identified by its authoritative `SourceEnvelope`
//! identity) to the artifacts produced by the `mncs-language` frontend for
//! that state. Snapshots are never mutated; a changed document simply gets a
//! new snapshot, making "is this result current?" answerable by fingerprint
//! comparison instead of bookkeeping.

use mncs_compiler::SourceFrontEndResult;
use mncs_syntax::{SourceDiagnostic, SourceEnvelope};

use crate::coords::PositionMap;
use crate::indexes::SymbolIndex;

/// Immutable analysis snapshot for one exact document state.
#[derive(Debug)]
pub struct DocumentAnalysis {
    pub uri: String,
    /// Authoritative `mncs:source:artifact:...` identity of the analyzed text.
    pub source_identity: String,
    /// Workspace generation at which this snapshot was produced.
    pub generation: u64,
    /// Source profile declared by the analyzed header.
    pub language_profile: String,
    /// Authoritative frontend artifacts for exactly this source state.
    pub front_end: SourceFrontEndResult,
    pub positions: PositionMap,
    pub symbols: SymbolIndex,
}

impl DocumentAnalysis {
    pub fn analyze(uri: &str, envelope: SourceEnvelope, generation: u64) -> Self {
        let language_profile = envelope.language_version.clone();
        let front_end = mncs_compiler::ReferenceCompiler::default().front_end(envelope);
        let text = front_end.envelope.text.clone();
        let positions = PositionMap::new(&text);
        let symbols = SymbolIndex::build(&front_end);
        Self {
            uri: uri.to_owned(),
            source_identity: front_end.envelope.identity.clone(),
            generation,
            language_profile,
            front_end,
            positions,
            symbols,
        }
    }

    /// The analyzed text (retained by the envelope).
    pub fn text(&self) -> &str {
        &self.front_end.envelope.text
    }

    /// Whether the authoritative frontend accepted the program.
    pub fn valid(&self) -> bool {
        self.front_end.is_valid()
    }

    /// All authoritative diagnostics for this snapshot.
    pub fn diagnostics(&self) -> &[SourceDiagnostic] {
        &self.front_end.diagnostics
    }
}
