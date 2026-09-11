# Visual Studio Code integration

The VS Code adapter is intentionally thin:

```text
canonical TextMate grammar + VS Code language registration
                         + standard vscode-languageclient
                                      ↓
                                  mncs-lsp
```

`package.sh` copies `integration/static-syntax/mncs.tmLanguage.json` into a
temporary extension staging directory at package time. This keeps the
TextMate grammar single-sourced. The extension registers `.mncs` as `MNCS`,
starts `mncs-lsp` on activation, and lets the server provide diagnostics,
hover, definitions, references, completion, semantic tokens, symbols, and
folding through standard LSP requests.

Install locally with:

```bash
integration/vscode/install.sh
```

The command resolver checks, in order, an explicit `mncs.languageServer.command`
setting, `MNCS_LSP`, the inherited `PATH`, `~/.local/bin`, `~/.cargo/bin`, and
`$XDG_DATA_HOME/bin`. GUI-launched VS Code therefore does not depend on an
interactive shell having sourced Cargo's environment. Override discovery in
Settings JSON when needed:

```json
{
  "mncs.languageServer.command": "/absolute/path/to/mncs-lsp",
  "mncs.languageServer.env": {
    "MNCS_LIBRARY_PATH": "/path/to/mncs-language/library"
  }
}
```

After installation, reload VS Code, open an `.mncs` file, and inspect
`Output → MNCS Language Server` if startup needs troubleshooting. The
extension does not implement semantic behavior and does not provide
formatting, rename, code actions, signature help, or other capabilities the
server intentionally does not advertise.
