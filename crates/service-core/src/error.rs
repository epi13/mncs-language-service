//! Error taxonomy for the MNCS Language Service core.

use std::fmt;

/// Errors surfaced by the resident service core.
///
/// The core distinguishes operational failures (documents that do not exist,
/// invalid requests) from semantic conservatism (unsupported, unresolved) so
/// protocol adapters can preserve those distinctions instead of collapsing
/// them into generic failures or empty successes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceError {
    /// The requested document is not known to the service.
    DocumentNotFound { uri: String },
    /// The workspace root does not exist or is unreadable.
    WorkspaceUnavailable { path: String },
    /// The request was malformed for the service's model (not a protocol
    /// concern).
    InvalidRequest { reason: String },
    /// A capability is genuinely unavailable for this input; the reason says
    /// which authoritative stage did not produce the required artifact.
    Unsupported { reason: String },
    /// The capability is supported but produced no confident subject.
    Unresolved { reason: String },
}

impl fmt::Display for ServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DocumentNotFound { uri } => write!(formatter, "document not found: {uri}"),
            Self::WorkspaceUnavailable { path } => {
                write!(formatter, "workspace unavailable: {path}")
            }
            Self::InvalidRequest { reason } => write!(formatter, "invalid request: {reason}"),
            Self::Unsupported { reason } => write!(formatter, "unsupported: {reason}"),
            Self::Unresolved { reason } => write!(formatter, "unresolved: {reason}"),
        }
    }
}

impl std::error::Error for ServiceError {}
