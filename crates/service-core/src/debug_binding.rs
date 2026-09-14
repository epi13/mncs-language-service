//! Shared source/debug identity projection.
//!
//! This module does not implement a debugger. It projects the exact source
//! snapshot and compiler-owned declaration/test identities into the vocabulary
//! consumed by `mncs-debug`, so LSP/MCP clients do not invent a second source
//! location model. Runtime operation and failure locations stay explicitly
//! unavailable until the compiler/runtime emits those facts.

use serde::{Deserialize, Serialize};

use crate::coords::RangeInfo;
use crate::queries::{ResponseStatus, SnapshotInfo};

pub const DEBUG_SOURCE_BINDING_SCHEMA_VERSION: &str = "mncs.debug-source-binding/1";

/// Capability vocabulary shared with `mncs-debug` and its integration
/// contract. A capability state is evidence about the current implementation,
/// not a promise that a client may emulate the missing semantic fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DebugCapabilityStatus {
    Supported,
    PartiallySupported,
    Unsupported,
    Emulated,
    BootstrapBoundary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DebugCapabilityState {
    pub status: DebugCapabilityStatus,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DebugBindingResolution {
    Exact,
    Derived,
    Unavailable,
}

/// Identity-bound source location shared by language-service and debugger
/// consumers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DebugSourceBinding {
    pub schema_version: String,
    /// The authoritative `mncs:source:artifact:<sha256>` identity.
    pub source_identity: String,
    pub uri: String,
    pub language_profile: String,
    pub module_name: String,
    pub module_identity: String,
    pub function_name: Option<String>,
    pub function_identity: Option<String>,
    /// Stable declaration identity for a first-class test, when applicable.
    pub test_declaration_identity: Option<String>,
    /// Body-sensitive test-case identity, when applicable.
    pub test_case_identity: Option<String>,
    /// The authoritative declaration span projected into byte and LSP
    /// coordinates. This is not claimed to be a runtime failure location.
    pub source_span: RangeInfo,
    pub symbol_resolution: DebugBindingResolution,
    /// Populated only when the compiler/runtime supplies a precise failing
    /// operation location. The current service intentionally leaves it empty.
    pub failure_location: Option<RangeInfo>,
    pub runtime_operation_identity: Option<String>,
    pub runtime_operation_resolution: DebugCapabilityState,
    pub failure_location_resolution: DebugCapabilityState,
    pub breakpoint_resolution: DebugCapabilityState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebugSourceBindingResponse {
    pub status: ResponseStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<SnapshotInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binding: Option<DebugSourceBinding>,
}
