//! Candidate analysis (Phase 4): isolated candidate snapshots and identity-
//! bound deltas against the resident baseline.
//!
//! A candidate is analyzed *without* publishing it to the workspace: the
//! baseline snapshot stays authoritative and untouched. Every delta is
//! computed by `mncs-language` itself (semantic diff via
//! [`mncs_model::Program::semantic_diff`], stale evidence via
//! [`mncs_model::Program::invalidation_from`], obligations via
//! `generate_obligations`), never by re-deriving language semantics here.
//!
//! Fail-closed rules:
//! - the baseline must elaborate; a broken baseline is `unsupported`;
//! - a candidate that does not elaborate yields diagnostics only, with the
//!   semantic/obligation deltas explicitly unavailable;
//! - identical candidate text answers with an explicit unchanged marker;
//! - nothing here mutates workspace state or promotes a candidate.

use std::sync::Arc;

use mncs_model::{IdentityChange, IdentityRecord, ObligationStatus};

use crate::analysis::DocumentAnalysis;
use crate::queries::{snapshot_info, LanguageService, ResponseStatus, SnapshotInfo, StatusCounts};
use crate::ServiceError;

/// One obligation-level observation used in candidate deltas.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CandidateObligation {
    pub identity: String,
    pub subject: String,
    pub requirement: String,
    pub status: String,
}

/// Semantic identity delta between baseline and candidate programs.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct SemanticDelta {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed: Vec<ChangedIdentity>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ChangedIdentity {
    pub identity: String,
    pub fingerprint_before: String,
    pub fingerprint_after: String,
}

/// Obligation status delta between baseline and candidate.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ObligationDelta {
    pub added: Vec<CandidateObligation>,
    pub removed: Vec<String>,
    #[serde(rename = "status_changed")]
    pub status_changed: Vec<ObligationStatusChange>,
    pub counts_baseline: StatusCounts,
    pub counts_candidate: StatusCounts,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ObligationStatusChange {
    pub identity: String,
    pub before: String,
    pub after: String,
}

/// Diagnostics delta between baseline and candidate frontends.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct DiagnosticsDelta {
    pub baseline_count: usize,
    pub candidate_count: usize,
    pub codes_added: Vec<String>,
    pub codes_removed: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct StaleEvidenceItem {
    pub evidence: String,
    pub dependency: String,
    pub reason: String,
}

/// Full identity-bound candidate analysis response.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CandidateAnalysisResponse {
    pub status: ResponseStatus,
    pub uri: String,
    pub baseline_snapshot: Option<SnapshotInfo>,
    pub baseline_source_identity: String,
    pub candidate_source_identity: String,
    /// False when the candidate text is byte-identical to the baseline.
    pub changed: bool,
    pub candidate_elaborates: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic: Option<SemanticDelta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub obligations: Option<ObligationDelta>,
    pub diagnostics: DiagnosticsDelta,
    /// Evidence the language reports as invalidated by this candidate.
    pub stale_evidence: Vec<StaleEvidenceItem>,
    /// Explicit unresolved conditions; never silently dropped.
    pub unresolved: Vec<String>,
}

fn status_label(status: &ObligationStatus) -> String {
    match status {
        ObligationStatus::Pass => "pass".to_owned(),
        ObligationStatus::Fail => "fail".to_owned(),
        ObligationStatus::Unknown => "unknown".to_owned(),
    }
}

fn obligation_map(
    analysis: &DocumentAnalysis,
) -> Option<std::collections::BTreeMap<String, CandidateObligation>> {
    let program = analysis.front_end.program.as_ref()?;
    let generation = program.generate_obligations();
    Some(
        generation
            .obligations
            .iter()
            .map(|obligation| {
                (
                    obligation.identity.0.clone(),
                    CandidateObligation {
                        identity: obligation.identity.0.clone(),
                        subject: obligation.subject.0.clone(),
                        requirement: obligation.requirement.0.clone(),
                        status: status_label(&obligation.status),
                    },
                )
            })
            .collect(),
    )
}

fn counts(obligations: &std::collections::BTreeMap<String, CandidateObligation>) -> StatusCounts {
    let mut counts = StatusCounts::default();
    for obligation in obligations.values() {
        match obligation.status.as_str() {
            "pass" => counts.pass += 1,
            "fail" => counts.fail += 1,
            _ => counts.unknown += 1,
        }
    }
    counts
}

fn diagnostics_codes(analysis: &DocumentAnalysis) -> Vec<String> {
    let mut codes: Vec<String> = analysis
        .diagnostics()
        .iter()
        .map(|diagnostic| diagnostic.code.clone())
        .collect();
    codes.sort();
    codes.dedup();
    codes
}

fn code_delta(before: &[String], after: &[String]) -> (Vec<String>, Vec<String>) {
    let added: Vec<String> = after
        .iter()
        .filter(|code| !before.contains(code))
        .cloned()
        .collect();
    let removed: Vec<String> = before
        .iter()
        .filter(|code| !after.contains(code))
        .cloned()
        .collect();
    (added, removed)
}

fn records_to_identities(records: &[IdentityRecord]) -> Vec<String> {
    records
        .iter()
        .map(|record| record.identity.0.clone())
        .collect()
}

impl LanguageService {
    /// Analyze proposed document content as an isolated candidate without
    /// mutating workspace state. The baseline stays authoritative.
    pub fn analyze_candidate(
        &self,
        uri: &str,
        candidate_text: &str,
    ) -> Result<CandidateAnalysisResponse, ServiceError> {
        // Ensure the document exists and the baseline is analyzed from its
        // current content; the candidate never enters the DocumentStore.
        self.store.ensure_loaded(uri)?;
        let baseline = self.snapshot(uri)?;
        let baseline_identity = baseline.source_identity.clone();

        let candidate_envelope = self.store.envelope(uri, candidate_text);
        let candidate_identity = candidate_envelope.identity.clone();
        let generation = self.store.generation();
        // The candidate elaborates against the same resident resolution the
        // baseline uses: its `use` targets resolve against workspace
        // documents and configured library roots, so editing an importing
        // module never produces false unresolvable-import diagnostics.
        let dependencies =
            crate::modules::DependencyFingerprints::collect(&self.store, candidate_text);
        let resolver = crate::modules::StoreResolver::new(&self.store);
        let candidate = Arc::new(DocumentAnalysis::analyze_with_resolver(
            uri,
            candidate_envelope,
            generation,
            dependencies,
            &resolver,
        ));

        if candidate_identity == baseline_identity {
            return Ok(CandidateAnalysisResponse {
                status: ResponseStatus::Answered,
                uri: uri.to_owned(),
                baseline_snapshot: Some(snapshot_info(uri, &baseline)),
                baseline_source_identity: baseline_identity,
                candidate_source_identity: candidate_identity,
                changed: false,
                candidate_elaborates: candidate.valid(),
                semantic: None,
                obligations: None,
                diagnostics: DiagnosticsDelta {
                    baseline_count: baseline.diagnostics().len(),
                    candidate_count: candidate.diagnostics().len(),
                    ..DiagnosticsDelta::default()
                },
                stale_evidence: Vec::new(),
                unresolved: vec!["candidate is identical to the baseline".to_owned()],
            });
        }

        let mut unresolved: Vec<String> = Vec::new();
        let (Some(baseline_program), Some(candidate_program)) = (
            baseline.front_end.program.as_ref(),
            candidate.front_end.program.as_ref(),
        ) else {
            if !baseline.valid() {
                return Err(ServiceError::Unsupported {
                    reason: "baseline does not elaborate; fix it before analyzing candidates"
                        .to_owned(),
                });
            }
            unresolved.push(
                "candidate does not elaborate; semantic and obligation deltas are unavailable"
                    .to_owned(),
            );
            let (codes_added, codes_removed) = code_delta(
                &diagnostics_codes(&baseline),
                &diagnostics_codes(&candidate),
            );
            return Ok(CandidateAnalysisResponse {
                status: ResponseStatus::Answered,
                uri: uri.to_owned(),
                baseline_snapshot: Some(snapshot_info(uri, &baseline)),
                baseline_source_identity: baseline_identity,
                candidate_source_identity: candidate_identity,
                changed: true,
                candidate_elaborates: false,
                semantic: None,
                obligations: None,
                diagnostics: DiagnosticsDelta {
                    baseline_count: baseline.diagnostics().len(),
                    candidate_count: candidate.diagnostics().len(),
                    codes_added,
                    codes_removed,
                },
                stale_evidence: Vec::new(),
                unresolved,
            });
        };

        // Authoritative language-owned comparisons.
        let diff = baseline_program.semantic_diff(candidate_program);
        let semantic_delta = SemanticDelta {
            added: records_to_identities(&diff.added),
            removed: records_to_identities(&diff.removed),
            changed: diff
                .changed
                .iter()
                .map(|change: &IdentityChange| ChangedIdentity {
                    identity: change.identity.0.clone(),
                    fingerprint_before: change.before.clone(),
                    fingerprint_after: change.after.clone(),
                })
                .collect(),
        };

        let (stale_evidence, invalidation_note) = match baseline_program
            .invalidation_from(candidate_program)
        {
            Ok(report) => {
                let items = report
                    .invalidated_evidence
                    .iter()
                    .map(|evidence| StaleEvidenceItem {
                        evidence: evidence.0.clone(),
                        dependency: String::new(),
                        reason: "evidence depends on identities this candidate changes".to_owned(),
                    })
                    .collect();
                (items, None)
            }
            Err(_) => (
                Vec::new(),
                Some(
                    "evidence invalidation requires a valid semantic graph on both sides"
                        .to_owned(),
                ),
            ),
        };
        if let Some(note) = invalidation_note {
            unresolved.push(note.to_owned());
        }

        let baseline_obligations = obligation_map(&baseline);
        let candidate_obligations = obligation_map(&candidate);
        let (obligation_delta, obligations_available) =
            match (&baseline_obligations, &candidate_obligations) {
                (Some(before), Some(after)) => {
                    let added = after
                        .values()
                        .filter(|item| !before.contains_key(&item.identity))
                        .cloned()
                        .collect();
                    let removed = before
                        .keys()
                        .filter(|identity| !after.contains_key(*identity))
                        .cloned()
                        .collect();
                    let status_changed = after
                        .values()
                        .filter_map(|item| {
                            let previous = before.get(&item.identity)?;
                            if previous.status == item.status {
                                return None;
                            }
                            Some(ObligationStatusChange {
                                identity: item.identity.clone(),
                                before: previous.status.clone(),
                                after: item.status.clone(),
                            })
                        })
                        .collect();
                    (
                        Some(ObligationDelta {
                            added,
                            removed,
                            status_changed,
                            counts_baseline: counts(before),
                            counts_candidate: counts(after),
                        }),
                        true,
                    )
                }
                _ => {
                    unresolved
                        .push("obligation comparison requires both sides to elaborate".to_owned());
                    (None, false)
                }
            };
        let _ = obligations_available;

        let (codes_added, codes_removed) = code_delta(
            &diagnostics_codes(&baseline),
            &diagnostics_codes(&candidate),
        );

        Ok(CandidateAnalysisResponse {
            status: ResponseStatus::Answered,
            uri: uri.to_owned(),
            baseline_snapshot: Some(snapshot_info(uri, &baseline)),
            baseline_source_identity: baseline_identity,
            candidate_source_identity: candidate_identity,
            changed: true,
            candidate_elaborates: candidate.valid(),
            semantic: Some(semantic_delta),
            obligations: obligation_delta,
            diagnostics: DiagnosticsDelta {
                baseline_count: baseline.diagnostics().len(),
                candidate_count: candidate.diagnostics().len(),
                codes_added,
                codes_removed,
            },
            stale_evidence,
            unresolved,
        })
    }
}
