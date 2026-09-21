//! Compact, identity-bound workspace change events.
//!
//! The event stream is deliberately a projection of resident Language Service
//! state, not a second semantic authority.  It carries enough information for
//! a continuous verifier to decide what is stale and which authoritative
//! consumer to invoke, while leaving source text and compiler-sized payloads
//! behind the query boundary.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex,
};

use mncs_model::{ObligationStatus, SemanticImpact};
use serde::{Deserialize, Serialize};

use crate::analysis::DocumentAnalysis;

pub const WORKSPACE_CHANGE_SCHEMA_VERSION: &str = "mncs.workspace-change/1";
pub const WORKSPACE_EVENT_CURSOR_SCHEMA_VERSION: &str = "mncs.workspace-event-cursor/1";
const DEFAULT_EVENT_CAPACITY: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceIdentity {
    pub uri: String,
    pub identity: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticSubjectChange {
    pub identity: String,
    pub change: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before_fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticDelta {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub added: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resolved: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceObligationDelta {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub added: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resolved: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub status_changed: Vec<String>,
    pub complete: bool,
}

/// One bounded change in the resident workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceChangeEvent {
    pub schema_version: String,
    /// Monotonic cursor assigned by the resident service, independent of the
    /// workspace generation.  Cursors let clients resume without replaying a
    /// repository or retaining every transient keystroke forever.
    pub cursor: u64,
    /// The generation whose state was replaced by this event.
    pub replaced_generation: u64,
    /// The generation represented by all identities in this event.
    pub current_generation: u64,
    pub current: SourceIdentity,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub affected_documents: Vec<SourceIdentity>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub semantic_subjects: Vec<SemanticSubjectChange>,
    pub diagnostics: DiagnosticDelta,
    pub obligations: WorkspaceObligationDelta,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub impact_identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub impact: Option<SemanticImpact>,
    /// False means the compiler could not establish a complete semantic
    /// envelope; consumers must preserve UNKNOWN rather than widening scope.
    pub impact_complete: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceEventCursor {
    pub schema_version: String,
    pub after_cursor: u64,
    pub current_cursor: u64,
    pub oldest_cursor: u64,
    pub reset_required: bool,
    #[serde(default)]
    pub events: Vec<WorkspaceChangeEvent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitations: Vec<String>,
}

#[derive(Debug)]
struct EventState {
    next_cursor: u64,
    events: VecDeque<WorkspaceChangeEvent>,
}

/// Bounded event history shared by every local client of one resident
/// service.  It is intentionally not a durable Store: successful consumers
/// retain evidence through Forge/Store boundaries, while transient edit
/// events may age out and force a cursor reset.
#[derive(Debug)]
pub struct EventHub {
    capacity: usize,
    state: Mutex<EventState>,
    /// Number of events discarded because the bounded history was full.
    discarded: AtomicU64,
}

impl Default for EventHub {
    fn default() -> Self {
        Self::new(DEFAULT_EVENT_CAPACITY)
    }
}

impl EventHub {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            state: Mutex::new(EventState {
                next_cursor: 0,
                events: VecDeque::new(),
            }),
            discarded: AtomicU64::new(0),
        }
    }

    pub fn push(&self, mut event: WorkspaceChangeEvent) -> u64 {
        let Ok(mut state) = self.state.lock() else {
            return 0;
        };
        state.next_cursor = state.next_cursor.saturating_add(1);
        event.cursor = state.next_cursor;
        if state.events.len() >= self.capacity {
            state.events.pop_front();
            self.discarded.fetch_add(1, Ordering::Relaxed);
        }
        state.events.push_back(event);
        state.next_cursor
    }

    pub fn current_cursor(&self) -> u64 {
        self.state
            .lock()
            .map(|state| state.next_cursor)
            .unwrap_or(0)
    }

    pub fn poll(&self, after_cursor: u64, max_events: usize) -> WorkspaceEventCursor {
        let Ok(state) = self.state.lock() else {
            return WorkspaceEventCursor {
                schema_version: WORKSPACE_EVENT_CURSOR_SCHEMA_VERSION.to_owned(),
                after_cursor,
                current_cursor: after_cursor,
                oldest_cursor: after_cursor.saturating_add(1),
                reset_required: true,
                events: Vec::new(),
                limitations: vec!["event hub state was unavailable".to_owned()],
            };
        };
        let oldest_cursor = state
            .events
            .front()
            .map(|event| event.cursor)
            .unwrap_or_else(|| state.next_cursor.saturating_add(1));
        let reset_required =
            after_cursor.saturating_add(1) < oldest_cursor && after_cursor < state.next_cursor;
        let events = if reset_required {
            Vec::new()
        } else {
            state
                .events
                .iter()
                .filter(|event| event.cursor > after_cursor)
                .take(max_events.max(1))
                .cloned()
                .collect()
        };
        let mut limitations = Vec::new();
        let discarded = self.discarded.load(Ordering::Relaxed);
        if discarded > 0 {
            limitations.push(format!(
                "{discarded} transient events have aged out of the bounded cursor history"
            ));
        }
        WorkspaceEventCursor {
            schema_version: WORKSPACE_EVENT_CURSOR_SCHEMA_VERSION.to_owned(),
            after_cursor,
            current_cursor: state.next_cursor,
            oldest_cursor,
            reset_required,
            events,
            limitations,
        }
    }
}

fn diagnostic_codes(analysis: Option<&DocumentAnalysis>) -> BTreeSet<String> {
    analysis
        .map(|analysis| {
            analysis
                .diagnostics()
                .iter()
                .map(|diagnostic| diagnostic.code.clone())
                .collect()
        })
        .unwrap_or_default()
}

fn status_label(status: &ObligationStatus) -> &'static str {
    match status {
        ObligationStatus::Pass => "pass",
        ObligationStatus::Fail => "fail",
        ObligationStatus::Unknown => "unknown",
    }
}

fn obligation_statuses(analysis: Option<&DocumentAnalysis>) -> Option<BTreeMap<String, String>> {
    let program = analysis?.front_end.program.as_ref()?;
    Some(
        program
            .generate_obligations()
            .obligations
            .into_iter()
            .map(|obligation| {
                (
                    obligation.identity.0,
                    status_label(&obligation.status).to_owned(),
                )
            })
            .collect(),
    )
}

/// Project two resident snapshots into the compact event contract.  This
/// function is intentionally conservative: an invalid or missing side keeps
/// the event observable but marks semantic impact incomplete.
pub fn project_change(
    uri: &str,
    replaced_generation: u64,
    current_generation: u64,
    before: Option<&DocumentAnalysis>,
    after: &DocumentAnalysis,
) -> WorkspaceChangeEvent {
    let mut limitations = Vec::new();
    let mut semantic_subjects = Vec::new();
    let mut impact = None;
    let mut impact_identity = None;
    let mut impact_complete = false;

    if let (Some(before_program), Some(after_program)) = (
        before.and_then(|analysis| analysis.front_end.program.as_ref()),
        after.front_end.program.as_ref(),
    ) {
        let diff = before_program.semantic_diff(after_program);
        for record in diff.added {
            semantic_subjects.push(SemanticSubjectChange {
                identity: record.identity.0,
                change: "added".to_owned(),
                before_fingerprint: None,
                after_fingerprint: Some(record.fingerprint),
            });
        }
        for record in diff.removed {
            semantic_subjects.push(SemanticSubjectChange {
                identity: record.identity.0,
                change: "removed".to_owned(),
                before_fingerprint: Some(record.fingerprint),
                after_fingerprint: None,
            });
        }
        let roots: Vec<_> = diff
            .changed
            .iter()
            .map(|change| change.identity.clone())
            .chain(
                semantic_subjects
                    .iter()
                    .filter(|subject| subject.change != "removed")
                    .map(|subject| mncs_model::SemanticId(subject.identity.clone())),
            )
            .collect();
        for change in diff.changed {
            semantic_subjects.push(SemanticSubjectChange {
                identity: change.identity.0,
                change: "changed".to_owned(),
                before_fingerprint: Some(change.before),
                after_fingerprint: Some(change.after),
            });
        }
        if let Ok(graph) = after_program.semantic_graph() {
            let projected = graph.impact_neighborhood(&roots, 2, 512);
            impact_complete = projected.complete;
            impact_identity = Some(projected.graph_identity.clone());
            impact = Some(projected);
        } else {
            limitations.push("the current program could not produce a semantic graph".to_owned());
        }
    } else {
        limitations.push(
            "semantic diff is unavailable because one workspace side did not elaborate".to_owned(),
        );
        limitations.push(
            "RAVEL/Test consumers must preserve UNKNOWN and must not widen to a full suite"
                .to_owned(),
        );
    }

    let before_codes = diagnostic_codes(before);
    let after_codes = diagnostic_codes(Some(after));
    let mut diagnostics = DiagnosticDelta {
        added: after_codes.difference(&before_codes).cloned().collect(),
        resolved: before_codes.difference(&after_codes).cloned().collect(),
    };
    diagnostics.added.sort();
    diagnostics.resolved.sort();

    let before_obligations = obligation_statuses(before);
    let after_obligations = obligation_statuses(Some(after));
    let mut obligations = WorkspaceObligationDelta::default();
    if let (Some(before_obligations), Some(after_obligations)) =
        (before_obligations.as_ref(), after_obligations.as_ref())
    {
        obligations.complete = true;
        obligations.added = after_obligations
            .keys()
            .filter(|identity| !before_obligations.contains_key(*identity))
            .cloned()
            .collect();
        obligations.resolved = before_obligations
            .keys()
            .filter(|identity| !after_obligations.contains_key(*identity))
            .cloned()
            .collect();
        obligations.status_changed = after_obligations
            .iter()
            .filter_map(|(identity, after_status)| {
                let before_status = before_obligations.get(identity)?;
                (before_status != after_status).then(|| identity.clone())
            })
            .collect();
    } else {
        limitations.push(
            "obligation delta is incomplete because one side has no valid program".to_owned(),
        );
    }
    obligations.added.sort();
    obligations.resolved.sort();
    obligations.status_changed.sort();

    semantic_subjects.sort_by(|left, right| {
        left.identity
            .cmp(&right.identity)
            .then(left.change.cmp(&right.change))
    });
    let current = SourceIdentity {
        uri: uri.to_owned(),
        identity: after.source_identity.clone(),
    };
    WorkspaceChangeEvent {
        schema_version: WORKSPACE_CHANGE_SCHEMA_VERSION.to_owned(),
        cursor: 0,
        replaced_generation,
        current_generation,
        current: current.clone(),
        affected_documents: vec![current],
        semantic_subjects,
        diagnostics,
        obligations,
        impact_identity,
        impact,
        impact_complete,
        limitations,
    }
}
