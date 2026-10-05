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

pub const WORKSPACE_CHANGE_SCHEMA_VERSION: &str = "mncs.workspace-change/2";
pub const WORKSPACE_EVENT_CURSOR_SCHEMA_VERSION: &str = "mncs.workspace-event-cursor/2";
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
    /// Explicit identity of the resident event stream.  A cursor is only
    /// meaningful together with this value; host restarts may restore the
    /// stream from the compact checkpoint or begin a new epoch.
    #[serde(default)]
    pub stream_identity: String,
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
    /// True when this event describes the exact current state reconstructed
    /// from the durable checkpoint or an authoritative disk refresh.
    #[serde(default)]
    pub reconciled: bool,
    /// Greatest earlier cursor explicitly superseded by this same-document
    /// reconciliation. Zero means no prior event is covered.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub supersedes_through_cursor: u64,
    /// The `current` identity is the last resident source identity when the
    /// document was removed from disk.
    #[serde(default)]
    pub removed: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitations: Vec<String>,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceEventCursor {
    pub schema_version: String,
    #[serde(default)]
    pub stream_identity: String,
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
    stream_identity: String,
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
        Self::new_with_stream(capacity, new_stream_identity())
    }

    pub fn new_with_stream(capacity: usize, stream_identity: String) -> Self {
        Self {
            capacity: capacity.max(1),
            state: Mutex::new(EventState {
                stream_identity,
                next_cursor: 0,
                events: VecDeque::new(),
            }),
            discarded: AtomicU64::new(0),
        }
    }

    /// Restore the compact stream checkpoint before any event is published.
    /// Existing in-memory events are never relabeled.
    pub fn restore_stream(&self, stream_identity: String, next_cursor: u64) {
        if let Ok(mut state) = self.state.lock() {
            if state.events.is_empty() {
                state.stream_identity = stream_identity;
                state.next_cursor = next_cursor;
            }
        }
    }

    /// Restore a durable event window only when it is internally consistent.
    /// Legacy checkpoints may have a stream high-water without retained
    /// events; those still resume at the exact high-water and explicitly
    /// require reconciliation for any older cursor.
    pub fn restore_checkpoint(
        &self,
        stream_identity: String,
        next_cursor: u64,
        events: Vec<WorkspaceChangeEvent>,
    ) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        if !state.events.is_empty() {
            return false;
        }
        let valid = events.len() <= self.capacity
            && events
                .iter()
                .all(|event| event.stream_identity == stream_identity)
            && events
                .windows(2)
                .all(|pair| pair[1].cursor == pair[0].cursor + 1)
            && events.iter().all(|event| event.cursor <= next_cursor)
            && events
                .last()
                .map(|event| event.cursor)
                .unwrap_or(next_cursor)
                == next_cursor;
        state.stream_identity = stream_identity;
        state.next_cursor = next_cursor;
        if valid {
            state.events = events.into_iter().collect();
            true
        } else {
            state.events.clear();
            false
        }
    }

    pub fn stream_identity(&self) -> String {
        self.state
            .lock()
            .map(|state| state.stream_identity.clone())
            .unwrap_or_default()
    }

    pub fn push(&self, mut event: WorkspaceChangeEvent) -> u64 {
        let Ok(mut state) = self.state.lock() else {
            return 0;
        };
        state.next_cursor = state.next_cursor.saturating_add(1);
        event.cursor = state.next_cursor;
        event.stream_identity = state.stream_identity.clone();
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

    /// Snapshot stream identity, high-water, and retained events atomically
    /// for the owning provider checkpoint.
    pub fn checkpoint_snapshot(&self) -> (String, u64, Vec<WorkspaceChangeEvent>) {
        self.state
            .lock()
            .map(|state| {
                (
                    state.stream_identity.clone(),
                    state.next_cursor,
                    state.events.iter().cloned().collect(),
                )
            })
            .unwrap_or_default()
    }

    /// Latest retained cursor that refers to this document. Consumers may
    /// discard earlier same-document deltas only when a later event carries
    /// this exact supersession bound and a complete current-state projection.
    pub fn latest_cursor_for_uri(&self, uri: &str) -> u64 {
        self.state
            .lock()
            .map(|state| {
                state
                    .events
                    .iter()
                    .filter(|event| {
                        event.current.uri == uri
                            || event
                                .affected_documents
                                .iter()
                                .any(|document| document.uri == uri)
                    })
                    .map(|event| event.cursor)
                    .max()
                    .unwrap_or(0)
            })
            .unwrap_or(0)
    }

    pub fn poll(&self, after_cursor: u64, max_events: usize) -> WorkspaceEventCursor {
        self.poll_for(None, after_cursor, max_events)
    }

    pub fn poll_for(
        &self,
        requested_stream_identity: Option<&str>,
        after_cursor: u64,
        max_events: usize,
    ) -> WorkspaceEventCursor {
        let Ok(state) = self.state.lock() else {
            return WorkspaceEventCursor {
                schema_version: WORKSPACE_EVENT_CURSOR_SCHEMA_VERSION.to_owned(),
                stream_identity: String::new(),
                after_cursor,
                current_cursor: after_cursor,
                oldest_cursor: after_cursor.saturating_add(1),
                reset_required: true,
                events: Vec::new(),
                limitations: vec!["event hub state was unavailable".to_owned()],
            };
        };
        // A nonzero cursor without its stream identity is not safe to resume:
        // the same number may belong to a different host epoch.  Initial
        // cursor-zero polling remains valid for compatibility clients.
        let stream_mismatch = requested_stream_identity
            .map(|requested| requested != state.stream_identity)
            .unwrap_or(after_cursor > 0);
        let oldest_cursor = state
            .events
            .front()
            .map(|event| event.cursor)
            .unwrap_or_else(|| state.next_cursor.saturating_add(1));
        // A durable consumer cursor can also be ahead of this stream after
        // restoring an older service snapshot. Do not return an empty page
        // that looks current: it must reconcile against the provider's exact
        // high-water mark before adopting this stream again.
        let cursor_out_of_range = after_cursor > state.next_cursor;
        let reset_required = stream_mismatch
            || cursor_out_of_range
            || (after_cursor.saturating_add(1) < oldest_cursor && after_cursor < state.next_cursor);
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
        if stream_mismatch {
            limitations.push(if requested_stream_identity.is_some() {
                "requested cursor belongs to a different Language Service event stream".to_owned()
            } else {
                "a nonzero cursor requires its Language Service event-stream identity".to_owned()
            });
        }
        if cursor_out_of_range {
            limitations.push(format!(
                "requested cursor {after_cursor} is ahead of stream high-water {}",
                state.next_cursor
            ));
        }
        WorkspaceEventCursor {
            schema_version: WORKSPACE_EVENT_CURSOR_SCHEMA_VERSION.to_owned(),
            stream_identity: state.stream_identity.clone(),
            after_cursor,
            current_cursor: state.next_cursor,
            oldest_cursor,
            reset_required,
            events,
            limitations,
        }
    }
}

fn new_stream_identity() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!("mnls-stream-{}-{nanos}", std::process::id())
}

#[cfg(test)]
mod cursor_tests {
    use super::*;

    fn event() -> WorkspaceChangeEvent {
        WorkspaceChangeEvent {
            schema_version: WORKSPACE_CHANGE_SCHEMA_VERSION.to_owned(),
            stream_identity: String::new(),
            cursor: 0,
            replaced_generation: 0,
            current_generation: 1,
            current: SourceIdentity {
                uri: "file:///workspace/example.mncs".to_owned(),
                identity: "mncs:source:example".to_owned(),
            },
            affected_documents: Vec::new(),
            semantic_subjects: Vec::new(),
            diagnostics: DiagnosticDelta::default(),
            obligations: WorkspaceObligationDelta::default(),
            impact_identity: None,
            impact: None,
            impact_complete: false,
            reconciled: false,
            supersedes_through_cursor: 0,
            removed: false,
            limitations: Vec::new(),
        }
    }

    #[test]
    fn expired_cursor_fails_closed_and_reports_replay_window() {
        let hub = EventHub::new_with_stream(2, "stream-one".to_owned());
        hub.push(event());
        hub.push(event());
        hub.push(event());

        let expired = hub.poll_for(Some("stream-one"), 0, 8);
        assert!(expired.reset_required);
        assert_eq!(expired.stream_identity, "stream-one");
        assert_eq!(expired.current_cursor, 3);
        assert_eq!(expired.oldest_cursor, 2);
        assert!(expired.events.is_empty());

        let retained = hub.poll_for(Some("stream-one"), 1, 8);
        assert!(!retained.reset_required);
        assert_eq!(
            retained
                .events
                .iter()
                .map(|item| item.cursor)
                .collect::<Vec<_>>(),
            [2, 3]
        );
    }

    #[test]
    fn restored_stream_keeps_identity_and_requires_reconciliation_for_lost_history() {
        let hub = EventHub::new_with_stream(2, "temporary".to_owned());
        hub.restore_stream("stream-restored".to_owned(), 10);

        let expired = hub.poll_for(Some("stream-restored"), 8, 8);
        assert!(expired.reset_required);
        assert_eq!(expired.current_cursor, 10);
        assert_eq!(expired.oldest_cursor, 11);
        let at_tip = hub.poll_for(Some("stream-restored"), 10, 8);
        assert!(!at_tip.reset_required);
        assert!(at_tip.events.is_empty());
    }

    #[test]
    fn persisted_window_replays_unacknowledged_events_after_restart() {
        let first = EventHub::new_with_stream(8, "stream-one".to_owned());
        first.push(event());
        let (stream, cursor, events) = first.checkpoint_snapshot();
        let resumed = EventHub::new_with_stream(8, "temporary".to_owned());
        assert!(resumed.restore_checkpoint(stream.clone(), cursor, events));

        let unacknowledged = resumed.poll_for(Some(&stream), 0, 8);
        assert!(!unacknowledged.reset_required);
        assert_eq!(unacknowledged.events.len(), 1);
        assert_eq!(unacknowledged.events[0].cursor, 1);

        let acknowledged = resumed.poll_for(Some(&stream), 1, 8);
        assert!(!acknowledged.reset_required);
        assert!(acknowledged.events.is_empty());
    }

    #[test]
    fn a_new_stream_never_relabels_an_old_cursor() {
        let hub = EventHub::new_with_stream(2, "stream-two".to_owned());
        hub.push(event());

        let reset = hub.poll_for(Some("stream-one"), 9, 8);
        assert!(reset.reset_required);
        assert_eq!(reset.stream_identity, "stream-two");
        assert_eq!(reset.current_cursor, 1);
        assert!(reset.events.is_empty());
    }

    #[test]
    fn a_cursor_ahead_of_restored_stream_requires_reconciliation() {
        let hub = EventHub::new_with_stream(4, "stream-restored".to_owned());
        hub.push(event());

        let reset = hub.poll_for(Some("stream-restored"), 7, 8);
        assert!(reset.reset_required);
        assert_eq!(reset.stream_identity, "stream-restored");
        assert_eq!(reset.current_cursor, 1);
        assert!(reset.events.is_empty());
        assert!(reset
            .limitations
            .iter()
            .any(|value| value.contains("ahead of stream high-water 1")));
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
    reconciled: bool,
) -> WorkspaceChangeEvent {
    let mut limitations = Vec::new();
    let mut semantic_subjects = Vec::new();
    let mut impact = None;
    let mut impact_identity = None;
    let mut impact_complete = false;

    if before.is_none() && !reconciled {
        if let Some(after_program) = after.front_end.program.as_ref() {
            let identities = after_program.semantic_identities();
            let roots: Vec<_> = identities
                .objects
                .iter()
                .map(|record| record.identity.clone())
                .collect();
            semantic_subjects.extend(identities.objects.into_iter().map(|record| {
                SemanticSubjectChange {
                    identity: record.identity.0,
                    change: "added".to_owned(),
                    before_fingerprint: None,
                    after_fingerprint: Some(record.fingerprint),
                }
            }));
            if let Ok(graph) = after_program.semantic_graph() {
                let projected = graph.impact_neighborhood(&roots, 2, 512);
                impact_complete = projected.complete;
                impact_identity = Some(projected.graph_identity.clone());
                impact = Some(projected);
            } else {
                limitations
                    .push("the current program could not produce a semantic graph".to_owned());
            }
        } else {
            limitations
                .push("new document did not elaborate into a valid semantic program".to_owned());
        }
    } else if let (Some(before_program), Some(after_program)) = (
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
    if before.is_none() && !reconciled {
        if let Some(after_obligations) = after_obligations.as_ref() {
            obligations.complete = true;
            obligations.added = after_obligations.keys().cloned().collect();
        }
    } else if let (Some(before_obligations), Some(after_obligations)) =
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
        stream_identity: String::new(),
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
        reconciled,
        supersedes_through_cursor: 0,
        removed: false,
        limitations,
    }
}

/// Project the exact removal of a previously analyzed disk document. The
/// current identity names the last source state that was resident before the
/// deletion; `removed` distinguishes that witness from live source content.
pub fn project_removal(
    uri: &str,
    replaced_generation: u64,
    current_generation: u64,
    before: &DocumentAnalysis,
    supersedes_through_cursor: u64,
) -> WorkspaceChangeEvent {
    let mut limitations = Vec::new();
    let mut semantic_subjects = Vec::new();
    let mut impact = None;
    let mut impact_identity = None;
    let mut impact_complete = false;
    if let Some(program) = before.front_end.program.as_ref() {
        let identities = program.semantic_identities();
        let roots: Vec<_> = identities
            .objects
            .iter()
            .map(|record| record.identity.clone())
            .collect();
        semantic_subjects.extend(identities.objects.into_iter().map(|record| {
            SemanticSubjectChange {
                identity: record.identity.0,
                change: "removed".to_owned(),
                before_fingerprint: Some(record.fingerprint),
                after_fingerprint: None,
            }
        }));
        if let Ok(graph) = program.semantic_graph() {
            let projected = graph.impact_neighborhood(&roots, 2, 512);
            impact_complete = projected.complete;
            impact_identity = Some(projected.graph_identity.clone());
            impact = Some(projected);
        } else {
            limitations
                .push("the last resident program could not produce a semantic graph".to_owned());
        }
    } else {
        limitations.push("the removed document had no valid resident semantic program".to_owned());
    }
    let obligations = match obligation_statuses(Some(before)) {
        Some(obligations) => WorkspaceObligationDelta {
            resolved: obligations.keys().cloned().collect(),
            complete: true,
            ..WorkspaceObligationDelta::default()
        },
        None => {
            limitations.push(
                "obligation removal is incomplete because the old program is unavailable"
                    .to_owned(),
            );
            WorkspaceObligationDelta::default()
        }
    };
    let current = SourceIdentity {
        uri: uri.to_owned(),
        identity: before.source_identity.clone(),
    };
    WorkspaceChangeEvent {
        schema_version: WORKSPACE_CHANGE_SCHEMA_VERSION.to_owned(),
        stream_identity: String::new(),
        cursor: 0,
        replaced_generation,
        current_generation,
        current: current.clone(),
        affected_documents: vec![current],
        semantic_subjects,
        diagnostics: DiagnosticDelta {
            resolved: diagnostic_codes(Some(before)).into_iter().collect(),
            ..DiagnosticDelta::default()
        },
        obligations,
        impact_identity,
        impact,
        impact_complete,
        reconciled: true,
        supersedes_through_cursor,
        removed: true,
        limitations,
    }
}
