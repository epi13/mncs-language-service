//! Lightweight drift checks for the KDE syntax-definition adapter.
//!
//! The actual XML parser is exercised by `integration/kde/validate.sh` with
//! KDE's bundled highlighter. These checks keep the committed definition tied
//! to the same authoritative keyword manifest as the TextMate grammar.

use mncs_static_syntax::scopes::KEYWORD_SPELLINGS;

fn kde_definition() -> String {
    std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../integration/kde/mncs.xml"),
    )
    .expect("KDE syntax definition is present")
}

#[test]
fn kde_definition_identifies_mncs_sources() {
    let xml = kde_definition();
    assert!(xml.contains("name=\"MNCS\""));
    assert!(xml.contains("section=\"Sources\""));
    assert!(xml.contains("extensions=\"*.mncs\""));
    assert!(xml.contains("mimetype=\"text/x-mncs\""));
    assert!(!xml.contains("foreground=") && !xml.contains("background="));
}

#[test]
fn every_authoritative_reserved_spelling_is_in_kde_keyword_list() {
    let xml = kde_definition();
    for (spelling, _) in KEYWORD_SPELLINGS {
        let item = format!("<item>{spelling}</item>");
        assert!(
            xml.contains(&item),
            "KDE keyword inventory lacks {spelling}"
        );
    }
}

#[test]
fn kde_definition_uses_semantic_default_styles() {
    let xml = kde_definition();
    for style in [
        "dsKeyword",
        "dsDataType",
        "dsFunction",
        "dsVariable",
        "dsConstant",
        "dsDecVal",
        "dsOperator",
        "dsComment",
    ] {
        assert!(xml.contains(style), "KDE definition lacks {style}");
    }
}
