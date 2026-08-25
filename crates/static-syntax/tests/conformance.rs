//! Conformance checks between the static grammar and the **authoritative**
//! `mncs-syntax` lexer.
//!
//! These tests run live against `mncs-language`'s exported lexer on every
//! execution, so any change upstream — a renamed or removed reserved word, a
//! reclassified operator, new literal forms — fails loudly here instead of
//! silently desynchronizing GitHub's view of MNCS from the compiler.
//!
//! Drift-protection layers (see `docs/github-language-support.md`):
//!
//! 1. **Compile-time**: [`scopes::expected_scope`] matches exhaustively over
//!    upstream `TokenKind`; adding/removing a variant breaks this build.
//! 2. **Runtime, bidirectional**: this file — every spelling the grammar
//!    treats as reserved must lex as that keyword kind upstream, and every
//!    authoritative classification observed on the probe corpus must be
//!    mirrored by the static grammar's scopes.
//! 3. **Manifest count invariant**: the number of distinct keyword kinds the
//!    live lexer produces over the probe corpus must equal the manifest size.

use mncs_static_syntax::scopes::{expected_scope, is_keyword_kind, KEYWORD_SPELLINGS};
use mncs_static_syntax::{load_grammar, tokenize_document, CANONICAL_EXTENSION};
use mncs_syntax::{SourceArtifactKind, SourceEnvelope, TokenKind};

/// A corpus exercising every authoritative lexical construct: all reserved
/// words, every operator and punctuation form, both comment styles (with
/// nesting), version and integer literals, record spread, and unicode
/// identifiers.
const PROBE: &str = r#"mncs 0.5;
// line comment with -> => == != <= >= symbols inside
/* block /* nested */ comment still open */ module probe.everything;
use lib.evidence;
record Pair { left: i64, right: bool }
enum Verdict { PASS, FAIL, UNKNOWN }
fn probe(alpha: i64, beta: bool) -> (result: i64)
    requires alpha_positive
    ensures result_bounded
    assumes inputs_total
    capability checked_integer
    effect write authorized_by ledger_mutation
{
    let doubled: i64 = alpha + alpha * 2 - 0;
    if beta != true {
        fail isolated;
    }
    if doubled >= 2 { } else { }
    if doubled < 2 { }
    if doubled <= 2 { }
    if doubled > 2 { }
    if doubled == 2 { }
    iterate steps up_to 3 carrying total: i64 = 0 {
        next total = total + 1;
    }
    let pair: Pair = Pair { ..pair_base, left: 1 };
    return match beta { true => while_marker(doubled), false => 0 };
}
fn évaluate(valeur: i64) -> (résultat: i64) { return valeur; }
while
"#;

fn authoritative_tokens(text: &str) -> Vec<(String, TokenKind)> {
    let envelope = SourceEnvelope::inline(SourceArtifactKind::Program, "probe", text);
    mncs_syntax::lex(&envelope)
        .tokens
        .into_iter()
        .map(|token| (token.text, token.kind))
        .collect()
}

/// The leaf scope covering the byte range of each authoritative non-trivia
/// token, computed from the grammar's token stream.
fn scope_for_span(
    lines: &[Vec<mncs_static_syntax::tokenizer::Token>],
    text: &str,
    start_byte: usize,
) -> Option<Vec<String>> {
    // Locate the line containing this offset.
    let mut consumed = 0usize;
    for (index, line) in text.split('\n').enumerate() {
        let line_len = line.len();
        if start_byte <= consumed + line_len {
            let local = start_byte - consumed;
            return lines[index]
                .iter()
                .filter(|token| {
                    token.start_byte <= local && local < token.start_byte + token.length
                })
                .max_by_key(|token| token.scopes.len())
                .map(|token| token.scopes.clone());
        }
        consumed += line_len + 1;
    }
    None
}

#[test]
fn every_manifest_spelling_lexes_as_its_authoritative_kind() {
    // Build one snippet per spelling so each word appears as its own token.
    for (spelling, expected_kind) in KEYWORD_SPELLINGS {
        let source = format!("mncs 0.5;\nmodule t;\n{spelling}\n");
        let tokens = authoritative_tokens(&source);
        assert!(
            tokens.iter().any(|(_, kind)| kind == expected_kind),
            "`{spelling}` no longer lexes as {expected_kind:?} upstream"
        );
    }
}

#[test]
fn every_upstream_keyword_kind_is_in_the_manifest() {
    // Any keyword-kind token produced by the live lexer over the probe corpus
    // must appear in the manifest with the same kind pairing.
    let tokens = authoritative_tokens(PROBE);
    let manifest_pairs: std::collections::BTreeSet<(String, TokenKind)> = KEYWORD_SPELLINGS
        .iter()
        .map(|(w, k)| ((*w).to_owned(), *k))
        .collect();
    for (text, kind) in &tokens {
        if is_keyword_kind(*kind) {
            assert!(
                manifest_pairs.contains(&(text.clone(), *kind)),
                "lexer classifies `{text}` as {kind:?}, but that pair is absent \
                 from KEYWORD_SPELLINGS — update the manifest and the grammar"
            );
        }
    }
}

#[test]
fn keyword_count_matches_the_authoritative_inventory() {
    let tokens = authoritative_tokens(PROBE);
    let distinct_kinds: std::collections::BTreeSet<TokenKind> = tokens
        .iter()
        .map(|(_, kind)| *kind)
        .filter(|kind| is_keyword_kind(*kind))
        .collect();
    assert_eq!(
        distinct_kinds.len(),
        KEYWORD_SPELLINGS.len(),
        "the live lexer exposes a different number of reserved-word kinds than \
         the grammar manifest knows about ({distinct_kinds:?})"
    );
}

#[test]
fn grammar_highlights_exactly_the_words_the_lexer_reserves() {
    let grammar = load_grammar().expect("grammar validates");
    let lines = tokenize_document(&grammar, PROBE).expect("tokenizes");
    let tokens = authoritative_tokens(PROBE);
    let mut offset = 0usize;

    for (text, kind) in tokens {
        let start = PROBE[offset..]
            .find(&text)
            .map_or(offset, |rel| offset + rel);
        offset = start + text.len();

        match kind {
            TokenKind::Whitespace | TokenKind::Unknown => continue,
            TokenKind::LineComment | TokenKind::BlockComment => continue, // covered below
            _ => {}
        }

        // Presentation choice, documented in the static-syntax README: bare
        // identifiers carry no scope (themes style declarations, calls, and
        // members instead), so uncovered identifiers are expected here.
        let Some(scopes) = scope_for_span(&lines, PROBE, start) else {
            if kind == TokenKind::Identifier {
                continue;
            }
            panic!("no grammar token covers `{text}` at byte {start}");
        };
        if scopes.is_empty() && kind == TokenKind::Identifier {
            continue;
        }

        let leaf = scopes.last().cloned().unwrap_or_default();
        if is_keyword_kind(kind) {
            assert!(
                leaf.starts_with("keyword.") || leaf.starts_with("constant.language."),
                "upstream reserves `{text}` ({kind:?}) but the grammar scoped it `{leaf}`"
            );
        } else if kind == TokenKind::Identifier {
            assert!(
                !leaf.starts_with("keyword."),
                "the grammar treats `{text}` as reserved (`{leaf}`) but the \
                 authoritative lexer classifies it as Identifier"
            );
        } else {
            // Presentation choice, documented in the static-syntax README:
            // dots inside a dotted module path belong to one namespace name,
            // so the grammar does not tokenize them separately.
            let inside_namespace = kind == TokenKind::Dot
                && scopes
                    .iter()
                    .any(|scope| scope == "entity.name.namespace.mncs");
            let expected = expected_scope(kind);
            if !expected.is_empty() && !inside_namespace {
                assert!(
                    scopes.iter().any(|scope| scope.starts_with(expected)),
                    "`{text}` lexes as {kind:?}; grammar should apply `{expected}` \
                     but found `{scopes:?}`"
                );
            }
        }
    }
}

#[test]
fn comments_are_scoped_as_comments() {
    let grammar = load_grammar().expect("grammar validates");
    let lines = tokenize_document(&grammar, PROBE).expect("tokenizes");
    let tokens = authoritative_tokens(PROBE);
    let mut offset = 0usize;
    for (text, kind) in tokens {
        let start = PROBE[offset..]
            .find(&text)
            .map_or(offset, |rel| offset + rel);
        offset = start + text.len();
        let expected_prefix = match kind {
            TokenKind::LineComment => "comment.line.double-slash.mncs",
            TokenKind::BlockComment => "comment.block.mncs",
            _ => continue,
        };
        // Comment spans may cover several lines; checking their first byte is
        // enough to detect a classification change.
        if let Some(scopes) = scope_for_span(&lines, PROBE, start) {
            assert!(
                scopes
                    .iter()
                    .any(|scope| scope.starts_with(expected_prefix)),
                "{kind:?} starting `{}` lost its comment scope: `{scopes:?}`",
                text.lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(24)
                    .collect::<String>()
            );
        }
    }
}

#[test]
fn every_supported_profile_header_is_recognized() {
    for profile in ["0.1", "0.2", "0.3", "0.4", "0.5"] {
        let source = format!("mncs {profile};\nmodule t;\n");
        let tokens = authoritative_tokens(&source);
        assert!(
            tokens
                .iter()
                .any(|(text, kind)| *kind == TokenKind::Version && text == profile),
            "profile {profile} no longer lexes as a Version token upstream"
        );

        let grammar = load_grammar().expect("grammar validates");
        let lines = tokenize_document(&grammar, &source).expect("tokenizes");
        let version_pos = source.find(profile).expect("version in source");
        let scopes = scope_for_span(&lines, &source, version_pos).expect("covered");
        assert!(
            scopes
                .iter()
                .any(|scope| scope.starts_with("constant.numeric.version.mncs")),
            "header version {profile} is not highlighted as a version literal"
        );
    }
}

#[test]
fn canonical_extension_still_holds() {
    assert_eq!(CANONICAL_EXTENSION, "mncs");
    let grammar = load_grammar().expect("grammar validates");
    assert_eq!(grammar.file_types, vec!["mncs".to_owned()]);
    assert_eq!(grammar.scope_name, "source.mncs");
}

#[test]
fn exhaustive_scope_mapping_covers_every_variant_observed() {
    // `expected_scope` is exhaustive at compile time; here we additionally
    // verify the probe corpus actually exercises every non-trivia family so
    // silent gaps cannot hide behind an unused arm.
    let tokens = authoritative_tokens(PROBE);
    let observed: std::collections::BTreeSet<TokenKind> =
        tokens.into_iter().map(|(_, kind)| kind).collect();
    for kind in observed {
        let _ = expected_scope(kind);
    }
    // And the families we depend on are present:
    for required in [
        TokenKind::MncsKeyword,
        TokenKind::AuthorizedKeyword,
        TokenKind::RecordKeyword,
        TokenKind::UpToKeyword,
        TokenKind::DotDot,
        TokenKind::FatArrow,
        TokenKind::Version,
    ] {
        assert!(
            authoritative_tokens(PROBE)
                .iter()
                .any(|(_, kind)| *kind == required),
            "probe corpus stopped exercising {required:?}"
        );
    }
}
