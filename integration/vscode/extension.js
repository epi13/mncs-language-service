const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const vscode = require("vscode");
const {
  LanguageClient,
} = require("vscode-languageclient/node");

let client;

function executable(pathname) {
  try {
    fs.accessSync(pathname, fs.constants.X_OK);
    return pathname;
  } catch {
    return undefined;
  }
}

function resolveServerCommand(configured) {
  if (path.isAbsolute(configured) || configured.includes(path.sep)) {
    return configured;
  }

  const candidates = [];
  if (process.env.MNCS_LSP) {
    candidates.push(process.env.MNCS_LSP);
  }
  const pathEntries = (process.env.PATH || "").split(path.delimiter);
  candidates.push(...pathEntries.map((entry) => path.join(entry, configured)));
  const dataHome = process.env.XDG_DATA_HOME || path.join(os.homedir(), ".local", "share");
  candidates.push(
    path.join(os.homedir(), ".local", "bin", configured),
    path.join(os.homedir(), ".cargo", "bin", configured),
    path.join(dataHome, "bin", configured),
  );

  return candidates.map(executable).find(Boolean) || configured;
}

function activate(context) {
  const settings = vscode.workspace.getConfiguration("mncs.languageServer");
  const command = resolveServerCommand(settings.get("command", "mncs-lsp"));
  const args = settings.get("args", []);
  const configuredEnv = settings.get("env", {});

  const serverOptions = {
    command,
    args,
    options: {
      env: { ...process.env, ...configuredEnv },
    },
  };
  const clientOptions = {
    documentSelector: [{ scheme: "file", language: "mncs" }],
    synchronize: {
      fileEvents: vscode.workspace.createFileSystemWatcher("**/*.mncs"),
    },
  };

  client = new LanguageClient(
    "mncsLanguageServer",
    "MNCS Language Server",
    serverOptions,
    clientOptions,
  );
  context.subscriptions.push(client);
  client.start();
}

async function deactivate() {
  if (client) {
    await client.stop();
  }
}

module.exports = { activate, deactivate };
