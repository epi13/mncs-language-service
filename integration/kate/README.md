# Kate integration

Kate uses KSyntaxHighlighting for static presentation and its LSP Client
plugin for resident semantic features. The checked-in
[`mncs.kateproject.example`](mncs.kateproject.example) is a portable project
configuration: copy it to the root of an MNCS project as `.kateproject`, then
open that project in Kate.

Install the shared KDE syntax definition first:

```bash
../kde/install.sh
```

Install the language service into a user-local directory from the repository
root when needed:

```bash
cargo install --path crates/lsp --locked --bin mncs-lsp --root "$HOME/.local"
```

In Kate, enable **LSP Client** under the Plugins tool view. The project file
starts `mncs-lsp` over stdio for `MNCS` documents and lets Kate discover the
project root. If the service is not found, check `command -v mncs-lsp` from a
desktop-launched shell or replace the command with an absolute executable
path.

The LSP adapter supplies diagnostics, hover, definitions, references,
document/workspace symbols, semantic tokens, completion, highlights, and
folding. The `.kateproject` file does not add formatting, rename, code-action,
signature-help, or inlay-hint behavior that the server does not advertise.
