# KDE / KSyntaxHighlighting integration

`mncs.xml` is the shallow KDE syntax definition for MNCS. It is consumed by
KWrite and Kate through KSyntaxHighlighting and uses KDE semantic default
styles (`dsKeyword`, `dsDataType`, `dsFunction`, `dsComment`, and so on), so
the active editor theme chooses the colors.

The definition recognizes `*.mncs` as `MNCS` in the `Sources` category. It
tracks the authoritative lexer vocabulary in
`crates/static-syntax/src/scopes.rs`; `cargo test -p mncs-static-syntax`
checks that every reserved spelling is present in this XML. Run the KDE-side
validator after installation to exercise the actual framework:

```bash
integration/kde/validate.sh
```

Install for the current user with:

```bash
integration/kde/install.sh
```

The standard KF6 location is
`$XDG_DATA_HOME/org.kde.syntax-highlighting/syntax/`, falling back to
`$HOME/.local/share/org.kde.syntax-highlighting/syntax/`. Restart KWrite or
Kate after installing so the syntax-definition index is refreshed.

KWrite uses KatePart/KSyntaxHighlighting for presentation, but current KWrite
does not ship Kate's application-level LSP Client plugin. Full semantic LSP
integration is therefore configured for Kate and VS Code; KWrite's supported
MNCS guarantee is syntax presentation.
