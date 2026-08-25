//! Authoritative-token ↔ static-scope correspondence.
//!
//! [`expected_scope`] maps **every** [`TokenKind`] variant exported by the
//! authoritative `mncs-syntax` lexer to the TextMate scope the static grammar
//! applies to it. The match is exhaustive on purpose: when `mncs-language`
//! adds, removes, or renames a token kind, this crate **fails to compile**,
//! which is the strongest available drift tripwire without an upstream
//! keyword-inventory API (see `docs/github-language-support.md`, "Upstream
//! asks").
//!
//! [`KEYWORD_SPELLINGS`] records the *textual* spelling of each reserved
//! word. Spellings live inside the upstream lexer's private match today, so
//! this manifest is the one unavoidable local copy; the conformance tests
//! verify every entry bidirectionally against the **live** lexer on every
//! run, so a rename upstream fails CI loudly.

use mncs_syntax::TokenKind;

/// Reserved-word spellings paired with their authoritative token kind, in
/// upstream declaration order.
pub const KEYWORD_SPELLINGS: &[(&str, TokenKind)] = &[
    ("mncs", TokenKind::MncsKeyword),
    ("module", TokenKind::ModuleKeyword),
    ("fn", TokenKind::FunctionKeyword),
    ("return", TokenKind::ReturnKeyword),
    ("let", TokenKind::LetKeyword),
    ("if", TokenKind::IfKeyword),
    ("else", TokenKind::ElseKeyword),
    ("fail", TokenKind::FailKeyword),
    ("requires", TokenKind::RequiresKeyword),
    ("ensures", TokenKind::EnsuresKeyword),
    ("assumes", TokenKind::AssumesKeyword),
    ("effect", TokenKind::EffectKeyword),
    ("capability", TokenKind::CapabilityKeyword),
    ("authorized_by", TokenKind::AuthorizedKeyword),
    ("enum", TokenKind::EnumKeyword),
    ("use", TokenKind::UseKeyword),
    ("record", TokenKind::RecordKeyword),
    ("match", TokenKind::MatchKeyword),
    ("iterate", TokenKind::IterateKeyword),
    ("up_to", TokenKind::UpToKeyword),
    ("carrying", TokenKind::CarryingKeyword),
    ("next", TokenKind::NextKeyword),
    ("while", TokenKind::WhileKeyword),
    ("true", TokenKind::TrueKeyword),
    ("false", TokenKind::FalseKeyword),
];

/// Whether the authoritative lexer classifies this kind as a reserved word
/// (as opposed to trivia, identifiers, literals, operators, or punctuation).
#[must_use]
pub fn is_keyword_kind(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::MncsKeyword
            | TokenKind::ModuleKeyword
            | TokenKind::FunctionKeyword
            | TokenKind::ReturnKeyword
            | TokenKind::LetKeyword
            | TokenKind::IfKeyword
            | TokenKind::ElseKeyword
            | TokenKind::FailKeyword
            | TokenKind::RequiresKeyword
            | TokenKind::EnsuresKeyword
            | TokenKind::AssumesKeyword
            | TokenKind::EffectKeyword
            | TokenKind::CapabilityKeyword
            | TokenKind::AuthorizedKeyword
            | TokenKind::EnumKeyword
            | TokenKind::UseKeyword
            | TokenKind::RecordKeyword
            | TokenKind::MatchKeyword
            | TokenKind::IterateKeyword
            | TokenKind::UpToKeyword
            | TokenKind::CarryingKeyword
            | TokenKind::NextKeyword
            | TokenKind::WhileKeyword
            | TokenKind::TrueKeyword
            | TokenKind::FalseKeyword
    )
}

/// The TextMate scope prefix family the static grammar must apply wherever
/// this token kind appears. Exhaustive over upstream `TokenKind`: adding a
/// variant breaks compilation here by design.
///
/// Identifiers are context-dependent and therefore have no single fixed
/// scope; the mapping reports `"identifier"` for them as a marker handled
/// specially by conformance checks.
#[must_use]
pub fn expected_scope(kind: TokenKind) -> &'static str {
    match kind {
        TokenKind::Whitespace => "",
        TokenKind::LineComment => "comment.line.double-slash.mncs",
        TokenKind::BlockComment => "comment.block.mncs",

        TokenKind::MncsKeyword => "keyword.other.header.mncs",
        TokenKind::ModuleKeyword => "keyword.declaration.module.mncs",
        TokenKind::FunctionKeyword => "keyword.declaration.function.mncs",
        TokenKind::ReturnKeyword => "keyword.control.flow.return.mncs",
        TokenKind::LetKeyword => "keyword.declaration.variable.mncs",
        TokenKind::IfKeyword => "keyword.control.flow.if.mncs",
        TokenKind::ElseKeyword => "keyword.control.flow.else.mncs",
        TokenKind::FailKeyword => "keyword.control.flow.fail.mncs",
        TokenKind::RequiresKeyword => "keyword.control.contract.requires.mncs",
        TokenKind::EnsuresKeyword => "keyword.control.contract.ensures.mncs",
        TokenKind::AssumesKeyword => "keyword.control.contract.assumes.mncs",
        TokenKind::EffectKeyword => "keyword.declaration.effect.mncs",
        TokenKind::CapabilityKeyword => "keyword.declaration.capability.mncs",
        TokenKind::AuthorizedKeyword => "keyword.control.authorized-by.mncs",
        TokenKind::EnumKeyword => "keyword.declaration.enum.mncs",
        TokenKind::UseKeyword => "keyword.declaration.mncs",
        TokenKind::RecordKeyword => "keyword.declaration.record.mncs",
        TokenKind::MatchKeyword => "keyword.control.flow.match.mncs",
        TokenKind::IterateKeyword => "keyword.control.iteration.iterate.mncs",
        TokenKind::UpToKeyword => "keyword.control.iteration.up-to.mncs",
        TokenKind::CarryingKeyword => "keyword.control.iteration.carrying.mncs",
        TokenKind::NextKeyword => "keyword.control.flow.next.mncs",
        TokenKind::WhileKeyword => "keyword.control.flow.while.mncs",
        TokenKind::TrueKeyword | TokenKind::FalseKeyword => "constant.language.boolean.mncs",

        TokenKind::Identifier => "identifier",

        TokenKind::Version => "constant.numeric.version.mncs",
        TokenKind::IntegerLiteral => "constant.numeric.integer.mncs",

        TokenKind::Plus
        | TokenKind::Minus
        | TokenKind::Star
        | TokenKind::Slash
        | TokenKind::Percent => "keyword.operator.arithmetic.mncs",
        TokenKind::EqEq
        | TokenKind::NotEq
        | TokenKind::Lt
        | TokenKind::Gt
        | TokenKind::Le
        | TokenKind::Ge => "keyword.operator.comparison.mncs",
        TokenKind::AndAnd | TokenKind::OrOr => "keyword.operator.logical.mncs",
        TokenKind::Equal => "keyword.operator.assignment.mncs",
        TokenKind::Arrow | TokenKind::FatArrow => "keyword.operator.arrow.mncs",

        TokenKind::Dot => "punctuation.accessor.dot.mncs",
        // `..` appears only inside record-literal spread in valid sources; in
        // other positions the grammar degrades to two accessor dots.
        TokenKind::DotDot => "keyword.operator.spread.mncs",
        TokenKind::Colon => "punctuation.separator.colon.mncs",
        TokenKind::Semicolon => "punctuation.terminator.statement.mncs",
        TokenKind::Comma => "punctuation.separator.comma.mncs",
        TokenKind::LeftParen => "punctuation.section.parentheses.begin.mncs",
        TokenKind::RightParen => "punctuation.section.parentheses.end.mncs",
        TokenKind::LeftBrace => "punctuation.section.block.begin.mncs",
        TokenKind::RightBrace => "punctuation.section.block.end.mncs",

        TokenKind::Unknown => "",
    }
}
