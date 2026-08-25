//! Real tokenization tests: run actual MNCS sources through the static
//! grammar via the bundled TextMate-subset engine and assert meaningful
//! scope classifications.
//!
//! These tests exercise the grammar the way GitHub/Linguist and editors
//! would: whole documents, including malformed sources that must still
//! highlight sanely.

use mncs_static_syntax::grammar::Grammar;
use mncs_static_syntax::tokenizer::Token;
use mncs_static_syntax::{load_grammar, tokenize_document};

fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crate lives two levels below repository root")
        .to_path_buf()
}

struct Doc {
    lines: Vec<String>,
    tokens: Vec<Vec<Token>>,
}

impl Doc {
    fn parse(text: &str) -> Self {
        let grammar = grammar();
        let tokens = tokenize_document(grammar, text).expect("tokenization");
        let mut lines: Vec<String> = text.split('\n').map(str::to_owned).collect();
        // Mirror the tokenizer's line splitting: a trailing newline does not
        // produce an extra (empty) line.
        if text.ends_with('\n') {
            lines.pop();
        }
        Self { lines, tokens }
    }

    /// Locate every whole-word occurrence of `needle` as
    /// `(line, byte_start)`, in document order.
    fn occurrences(&self, needle: &str) -> Vec<(usize, usize)> {
        assert!(!needle.is_empty(), "needle must be non-empty");
        let word_char = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_' || !byte.is_ascii();
        let needle_starts_word = needle.as_bytes()[0].is_ascii_alphanumeric()
            || needle.as_bytes()[0] == b'_'
            || !needle.as_bytes()[0].is_ascii();
        let needle_ends_word = {
            let last = needle.as_bytes()[needle.len() - 1];
            last.is_ascii_alphanumeric() || last == b'_' || !last.is_ascii()
        };
        let mut found = Vec::new();
        for (line_index, line) in self.lines.iter().enumerate() {
            let bytes = line.as_bytes();
            let mut search = 0usize;
            while let Some(rel) = line[search..].find(needle) {
                let start = search + rel;
                let end = start + needle.len();
                // Enforce word boundaries only where the needle itself is
                // word-like, so punctuation needles still match next to
                // identifiers (`Pass,`) while `n` inside `fn` does not.
                let before_ok = !needle_starts_word || start == 0 || !word_char(bytes[start - 1]);
                let after_ok = !needle_ends_word || end >= bytes.len() || !word_char(bytes[end]);
                if before_ok && after_ok {
                    found.push((line_index, start));
                }
                search += rel + needle.len();
            }
        }
        found
    }

    /// Tokens overlapping the span at `(line, start)` of length `len` —
    /// includes capture tokens and content runs wider than the needle.
    fn tokens_within(&self, line: usize, start: usize, len: usize) -> Vec<&Token> {
        let end = start + len;
        self.tokens[line]
            .iter()
            .filter(|token| {
                let token_end = token.start_byte + token.length;
                token.start_byte < end && token_end > start
            })
            .collect()
    }

    /// Assert the first occurrence of `needle` carries `scope_prefix`.
    fn assert_scope(&self, needle: &str, scope_prefix: &str) -> &Self {
        self.assert_scope_occurrence(needle, 0, scope_prefix)
    }

    /// Assert the nth occurrence (document order) of `needle` carries
    /// `scope_prefix`.
    fn assert_scope_occurrence(
        &self,
        needle: &str,
        occurrence: usize,
        scope_prefix: &str,
    ) -> &Self {
        let all = self.occurrences(needle);
        let Some(&(line, start)) = all.get(occurrence) else {
            panic!(
                "occurrence {occurrence} of `{needle}` not found; total {}",
                all.len()
            );
        };
        let tokens = self.tokens_within(line, start, needle.len());
        assert!(
            tokens
                .iter()
                .any(|token| token.has_scope_prefix(scope_prefix)),
            "expected occurrence {occurrence} of `{needle}` to carry a scope starting \
             with `{scope_prefix}`; scopes there: {:?}",
            tokens
                .iter()
                .map(|token| token.leaf_scope().unwrap_or("<unscoped>"))
                .collect::<Vec<_>>(),
        );
        self
    }

    /// Assert NO occurrence of `needle` anywhere carries `scope_prefix`.
    fn assert_not_scope(&self, needle: &str, scope_prefix: &str) -> &Self {
        for (line, start) in self.occurrences(needle) {
            assert!(
                !self
                    .tokens_within(line, start, needle.len())
                    .iter()
                    .any(|token| token.has_scope_prefix(scope_prefix)),
                "expected `{needle}` NOT to carry a scope starting with `{scope_prefix}`",
            );
        }
        self
    }
}

fn grammar() -> &'static Grammar {
    use std::sync::OnceLock;
    static GRAMMAR: OnceLock<Grammar> = OnceLock::new();
    GRAMMAR.get_or_init(|| load_grammar().expect("bundled grammar validates"))
}

fn fixture(rel: &str) -> String {
    let path = repo_root().join("tests/fixtures").join(rel);
    std::fs::read_to_string(path).expect("fixture readable")
}

fn sample(name: &str) -> String {
    let path = repo_root()
        .join("integration/github-linguist/samples")
        .join(name);
    std::fs::read_to_string(path).expect("sample readable")
}

// ---------------------------------------------------------------------------
// Header, module, declarations
// ---------------------------------------------------------------------------

#[test]
fn header_and_module_are_classified() {
    let doc = Doc::parse("mncs 0.5;\nmodule mncs.core.status.v1;\n");
    doc.assert_scope("mncs", "keyword.other.header.mncs")
        .assert_scope("0.5", "constant.numeric.version.mncs")
        .assert_scope("module", "keyword.declaration.module.mncs")
        .assert_scope("mncs.core.status.v1", "entity.name.namespace.mncs")
        .assert_scope(";", "punctuation.terminator.statement.mncs");
}

#[test]
fn enum_declaration_classifies_keyword_name_and_variants() {
    let doc = Doc::parse("mncs 0.3;\nmodule examples.finite;\nenum Verdict { Pass, Fail, Skip }\n");
    doc.assert_scope("enum", "keyword.declaration.enum.mncs")
        .assert_scope("Verdict", "entity.name.enum.mncs")
        .assert_scope("Pass", "entity.name.variant.mncs")
        .assert_scope("Skip", "entity.name.variant.mncs")
        .assert_scope(",", "punctuation.separator.comma.mncs")
        .assert_scope("{", "punctuation.section.block.begin.mncs")
        .assert_scope("}", "punctuation.section.block.end.mncs");
}

#[test]
fn record_declaration_classifies_fields_and_types() {
    let doc = Doc::parse(
        "mncs 0.5;\nmodule examples.records;\nrecord ClaimVerdict { status: Verdict, reason: UnknownReason }\n",
    );
    doc.assert_scope("record", "keyword.declaration.record.mncs")
        .assert_scope("ClaimVerdict", "entity.name.record.mncs")
        .assert_scope("status", "variable.other.member.mncs")
        .assert_scope("Verdict", "entity.name.type.mncs")
        .assert_scope("reason", "variable.other.member.mncs")
        .assert_scope("UnknownReason", "entity.name.type.mncs");
}

#[test]
fn function_signature_classifies_name_parameters_and_arrow() {
    let doc = Doc::parse(
        "mncs 0.2;\nmodule examples.signature;\nfn bounded_step(n: i64, limit: i64) -> (result: i64) { return n; }\n",
    );
    doc.assert_scope("fn", "keyword.declaration.function.mncs")
        .assert_scope("bounded_step", "entity.name.function.mncs")
        .assert_scope("n", "variable.parameter.mncs")
        .assert_scope("i64", "entity.name.type.mncs")
        .assert_scope("->", "keyword.operator.arrow.mncs")
        .assert_scope("result", "variable.parameter.mncs")
        .assert_scope("(", "punctuation.section.parentheses.begin.mncs")
        .assert_scope(")", "punctuation.section.parentheses.end.mncs");
}

#[test]
fn multi_line_signature_stays_in_signature_context() {
    let source = fixture("lineage/synthetic-lineage-g0.mncs");
    let doc = Doc::parse(&source);
    // Parameters of `dependencies_unchanged` continue across several lines.
    doc.assert_scope("parent_frozen", "variable.parameter.mncs");
    doc.assert_scope("development_recipe_current", "variable.parameter.mncs");
    doc.assert_scope("unchanged", "variable.parameter.mncs");
}

// ---------------------------------------------------------------------------
// Contracts, capabilities, effects
// ---------------------------------------------------------------------------

#[test]
fn contracts_capabilities_effects_are_classified() {
    // `flagship` carries a `requires` clause and a capability.
    let flagship = Doc::parse(&sample("flagship.mncs"));
    flagship
        .assert_scope("requires", "keyword.control.contract.requires.mncs")
        .assert_scope("n_within_limit", "entity.name.contract.mncs")
        .assert_scope("capability", "keyword.declaration.capability.mncs")
        .assert_scope("checked_integer", "entity.name.capability.mncs");

    // The lineage sample carries multiple `ensures` clauses.
    let lineage = Doc::parse(&sample("synthetic-lineage-g0.mncs"));
    lineage
        .assert_scope("ensures", "keyword.control.contract.ensures.mncs")
        .assert_scope(
            "changed_dependency_forces_reevaluation",
            "entity.name.contract.mncs",
        );

    // `cre3` exercises capabilities and an effect authorization.
    let retry = Doc::parse(&sample("cre3-retry-authority.mncs"));
    retry
        .assert_scope("capability", "keyword.declaration.capability.mncs")
        .assert_scope("retry_authority", "entity.name.capability.mncs")
        .assert_scope("effect", "keyword.declaration.effect.mncs")
        // Occurrence 0 of `observe` is the function name; 1 is effect kind.
        .assert_scope_occurrence("observe", 1, "entity.name.effect.mncs")
        .assert_scope("authorized_by", "keyword.control.authorized-by.mncs");

    // The semantic-error fixture keeps highlighting despite the violation.
    let effects = Doc::parse(&fixture("semantic-error.mncs"));
    effects
        .assert_scope("effect", "keyword.declaration.effect.mncs")
        .assert_scope("write", "entity.name.effect.mncs")
        .assert_scope("authorized_by", "keyword.control.authorized-by.mncs")
        .assert_scope("ledger_mutation", "entity.name.capability.mncs");
}

// ---------------------------------------------------------------------------
// Statements and control flow
// ---------------------------------------------------------------------------

#[test]
fn let_if_else_return_fail_are_classified() {
    let doc = Doc::parse(&sample("flagship.mncs"));
    doc.assert_scope("if", "keyword.control.flow.if.mncs")
        .assert_scope("fail", "keyword.control.flow.fail.mncs");
    // `isolated` is an ordinary identifier: it must NOT get keyword scope.
    doc.assert_not_scope("isolated", "keyword.");
    let records = Doc::parse(&sample("profile05-record-values.mncs"));
    records
        .assert_scope("let", "keyword.declaration.variable.mncs")
        .assert_scope("return", "keyword.control.flow.return.mncs");
    let branching = Doc::parse(concat!(
        "mncs 0.5;\nmodule t;\nfn f(ok: bool) -> (r: i32) { ",
        "if ok { return 1; } else { return 0; } }\n"
    ));
    branching.assert_scope("else", "keyword.control.flow.else.mncs");
}

#[test]
fn let_binding_names_types_and_assignment_are_classified() {
    let source =
        "mncs 0.2;\nmodule t;\nfn f(n: i64) -> (r: i64) { let next: i64 = n + 1; return next; }\n";
    let doc = Doc::parse(source);
    doc.assert_scope("next", "variable.name.mncs")
        .assert_scope("i64", "entity.name.type.mncs")
        .assert_scope("=", "keyword.operator.assignment.mncs")
        .assert_scope("+", "keyword.operator.arithmetic.mncs");
}

#[test]
fn bounded_iteration_is_fully_classified() {
    let doc = Doc::parse(&fixture("bounded-iteration.mncs"));
    doc.assert_scope("iterate", "keyword.control.iteration.iterate.mncs")
        .assert_scope("up_to", "keyword.control.iteration.up-to.mncs")
        // Occurrence 1 skips the `4` inside the `0.4` header version.
        .assert_scope_occurrence("4", 1, "constant.numeric.integer.mncs")
        .assert_scope("carrying", "keyword.control.iteration.carrying.mncs")
        .assert_scope("current", "variable.name.mncs")
        .assert_scope("next", "keyword.control.flow.next.mncs")
        .assert_scope("attempts", "variable.name.mncs");
}

#[test]
fn boolean_literals_are_constant_language() {
    let doc = Doc::parse(&fixture("lineage/synthetic-lineage-g0.mncs"));
    doc.assert_scope("true", "constant.language.boolean.mncs")
        .assert_scope("false", "constant.language.boolean.mncs");
}

#[test]
fn match_arms_classify_variant_and_arrow() {
    let doc = Doc::parse(&fixture("finite-match.mncs"));
    doc.assert_scope("match", "keyword.control.flow.match.mncs")
        .assert_scope("Pass", "entity.name.variant.mncs")
        .assert_scope("=>", "keyword.operator.arrow.mncs");
}

#[test]
fn comparison_operators_are_classified() {
    let doc = Doc::parse(&fixture("finite-match.mncs"));
    doc.assert_scope(">=", "keyword.operator.comparison.mncs");
}

#[test]
fn arithmetic_and_logical_operators_are_classified() {
    let doc = Doc::parse(
        "mncs 0.6; module operators; fn f(a: i64, b: bool) -> (r: i64) {\n\
         let n: i64 = a / 2 % 2; if b && true || false { return n; } return n; }",
    );
    doc.assert_scope("/", "keyword.operator.arithmetic.mncs")
        .assert_scope("%", "keyword.operator.arithmetic.mncs")
        .assert_scope("&&", "keyword.operator.logical.mncs")
        .assert_scope("||", "keyword.operator.logical.mncs");
}

// ---------------------------------------------------------------------------
// Record literals and member access
// ---------------------------------------------------------------------------

#[test]
fn record_literal_with_spread_is_classified() {
    let doc = Doc::parse(&sample("profile05-record-values.mncs"));
    doc.assert_scope("..", "keyword.operator.spread.mncs");
    // Field access after a dot is member/property access.
    doc.assert_scope(".celsius", "punctuation.accessor.dot.mncs");
    // Occurrence 0 is the declaration; the literal's type name gets
    // entity.name.type.
    doc.assert_scope_occurrence("Reading", 0, "entity.name.record.mncs");
    doc.assert_scope_occurrence("Reading", 1, "entity.name.type.mncs");
    // Field name inside the literal is a member.
    doc.assert_scope("celsius", "variable.other.member.mncs");
}

#[test]
fn variant_access_after_dot_is_property() {
    let doc = Doc::parse(&fixture("finite-match.mncs"));
    // Occurrence 0 of `Pass` is the enum declaration's variant; occurrence 1
    // is the use site `Verdict.Pass`, which is member access.
    doc.assert_scope_occurrence("Pass", 0, "entity.name.variant.mncs");
    doc.assert_scope_occurrence("Pass", 1, "variable.other.property.mncs");
}

#[test]
fn call_sites_are_variable_function() {
    let doc = Doc::parse(&fixture("valid-contracts.mncs"));
    // Occurrence 0 is the declaration site; later occurrences are call sites,
    // classified as function invocations.
    doc.assert_scope_occurrence("bounded_step", 0, "entity.name.function.mncs");
    doc.assert_scope_occurrence("bounded_step", 1, "variable.function.mncs");
}

// ---------------------------------------------------------------------------
// Comments
// ---------------------------------------------------------------------------

#[test]
fn line_comments_are_scoped() {
    let doc = Doc::parse("// top note\nmncs 0.5;\n// trailing note\nmodule t;\n");
    doc.assert_scope("//", "comment.line.double-slash.mncs");
}

#[test]
fn block_comments_are_scoped_across_lines() {
    let doc = Doc::parse("/* first\nsecond */\nmncs 0.5;\nmodule t;\n");
    doc.assert_scope("/*", "comment.block.mncs");
    doc.assert_scope("*/", "comment.block.mncs");
}

#[test]
fn nested_block_comments_remain_comment_up_to_supported_depth() {
    let doc = Doc::parse("/* outer /* inner */ still comment */\nmncs 0.5;\nmodule t;\n");
    // With nesting support, everything through the final `*/` is comment.
    doc.assert_scope("still", "comment.block.mncs");
    doc.assert_scope("comment", "comment.block.mncs");
    // And the code after the comment is not swallowed.
    doc.assert_scope("module", "keyword.declaration.module.mncs");
}

// ---------------------------------------------------------------------------
// Literals and numbers
// ---------------------------------------------------------------------------

#[test]
fn integers_do_not_swallow_following_punctuation() {
    let doc = Doc::parse("mncs 0.5;\nmodule t;\nrecord R { v: i32 }\nfn f(n: i32) -> (r: i32) { iterate s up_to 2 carrying acc: i32 = 0 { next acc = acc + 1; } return match n { 0 => 0 }; }\n");
    doc.assert_scope("2", "constant.numeric.integer.mncs");
    doc.assert_scope("1", "constant.numeric.integer.mncs");
}

// ---------------------------------------------------------------------------
// Malformed / partially highlightable sources
// ---------------------------------------------------------------------------

#[test]
fn syntax_error_fixture_still_highlights_keywords_and_comments() {
    let doc = Doc::parse(&fixture("syntax-error.mncs"));
    doc.assert_scope("fn", "keyword.declaration.function.mncs")
        .assert_scope("return", "keyword.control.flow.return.mncs")
        .assert_scope("identity", "entity.name.function.mncs")
        .assert_scope("value", "variable.parameter.mncs");
}

#[test]
fn semantic_error_fixture_still_highlights_constructs() {
    let doc = Doc::parse(&fixture("semantic-error.mncs"));
    doc.assert_scope("effect", "keyword.declaration.effect.mncs")
        .assert_scope("transfer", "entity.name.function.mncs")
        .assert_scope("amount", "variable.parameter.mncs");
}

#[test]
fn unclosed_region_degrades_without_panicking() {
    let doc =
        Doc::parse("mncs 0.5;\nmodule t;\nfn broken(x: i32) -> (r: i32) { if x > 0 { return 1;\n");
    doc.assert_scope("if", "keyword.control.flow.if.mncs")
        .assert_scope("return", "keyword.control.flow.return.mncs");
}

#[test]
fn stray_close_brace_is_punctuation() {
    let doc = Doc::parse("}\n");
    doc.assert_scope("}", "punctuation.section.block.end.mncs");
}

// ---------------------------------------------------------------------------
// Unicode identifiers (matches authoritative lexer's unicode alphabetic rule)
// ---------------------------------------------------------------------------

#[test]
fn unicode_identifiers_tokenize() {
    let doc = Doc::parse(&fixture("unicode.mncs"));
    doc.assert_scope("évaluate", "entity.name.function.mncs")
        .assert_scope("résultat", "variable.parameter.mncs");
}

// ---------------------------------------------------------------------------
// Ecosystem corpus: every curated Linguist sample must tokenize with sane
// classifications end-to-end.
// ---------------------------------------------------------------------------

#[test]
fn every_curated_sample_tokenizes_with_expected_landmarks() {
    let cases: &[(&str, &[&str], &[&str])] = &[
        (
            "flagship.mncs",
            &[
                "keyword.declaration.function.mncs",
                "keyword.control.flow.if.mncs",
            ],
            &["requires"],
        ),
        (
            "profile05-record-values.mncs",
            &[
                "keyword.declaration.record.mncs",
                "keyword.operator.spread.mncs",
            ],
            &["record"],
        ),
        (
            "cre3-retry-authority.mncs",
            &[
                "keyword.declaration.capability.mncs",
                "keyword.declaration.effect.mncs",
                "keyword.control.authorized-by.mncs",
                "keyword.control.flow.match.mncs",
            ],
            &["capability"],
        ),
        (
            "core-status.mncs",
            &[
                "keyword.declaration.enum.mncs",
                "entity.name.namespace.mncs",
            ],
            &["module"],
        ),
        (
            "core-ordering.mncs",
            &[
                "keyword.declaration.function.mncs",
                "keyword.operator.comparison.mncs",
                "keyword.control.flow.if.mncs",
            ],
            &["clamp"],
        ),
        (
            "ravel-core.mncs",
            &[
                "keyword.declaration.record.mncs",
                "keyword.control.flow.match.mncs",
                "entity.name.variant.mncs",
            ],
            &["Disposition"],
        ),
        (
            "synthetic-lineage-g0.mncs",
            &[
                "keyword.control.contract.requires.mncs",
                "keyword.declaration.record.mncs",
                "variable.other.member.mncs",
                "entity.name.variant.mncs",
            ],
            &["requires"],
        ),
    ];
    for (name, prefixes, landmark_needles) in cases {
        let doc = Doc::parse(&sample(name));
        for prefix in *prefixes {
            assert!(
                doc.tokens
                    .iter()
                    .flatten()
                    .any(|token| token.has_scope_prefix(prefix)),
                "{name}: expected at least one token scoped `{prefix}`"
            );
        }
        for needle in *landmark_needles {
            assert!(
                !doc.occurrences(needle).is_empty(),
                "{name}: landmark `{needle}` not present in token stream"
            );
        }
    }
}
