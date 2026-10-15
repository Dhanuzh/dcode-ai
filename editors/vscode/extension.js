// dcode-ai VS Code extension: launches `dcode-ai web` for the current
// workspace and embeds the local chat page in a webview panel.
//
// Plain JavaScript on purpose — no build step, no dependencies. The whole
// product lives in the Rust CLI; this file only starts it and shows its page.

"use strict";

const vscode = require("vscode");
const { spawn } = require("child_process");

/** @type {{ child: import("child_process").ChildProcess, url: string } | null} */
let running = null;

function activate(context) {
  context.subscriptions.push(
    vscode.commands.registerCommand("dcode-ai.openChat", openChat)
  );
}

async function openChat() {
  const folder = vscode.workspace.workspaceFolders?.[0];
  if (!folder) {
    vscode.window.showErrorMessage("dcode-ai: open a folder first.");
    return;
  }

  let url;
  try {
    url = await ensureServer(folder.uri.fsPath);
  } catch (err) {
    vscode.window.showErrorMessage(`dcode-ai: ${err.message || err}`);
    return;
  }

  const panel = vscode.window.createWebviewPanel(
    "dcodeAiChat",
    "dcode-ai",
    vscode.ViewColumn.Beside,
    { enableScripts: true, retainContextWhenHidden: true }
  );
  panel.webview.html = `<!doctype html>
<html>
<head><meta charset="utf-8">
<style>html,body,iframe{margin:0;padding:0;border:0;width:100%;height:100%;}</style>
</head>
<body><iframe src="${url}" allow="clipboard-read; clipboard-write"></iframe></body>
</html>`;
}

/**
 * Start `dcode-ai web` (once) in the workspace and resolve the tokenized
 * local URL it prints at startup.
 * @param {string} cwd
 * @returns {Promise<string>}
 */
function ensureServer(cwd) {
  if (running && running.child.exitCode === null) {
    return Promise.resolve(running.url);
  }
  running = null;

  const config = vscode.workspace.getConfiguration("dcode-ai");
  const binary = config.get("binaryPath", "dcode-ai");
  const extraArgs = config.get("extraArgs", []);

  return new Promise((resolve, reject) => {
    const child = spawn(binary, ["web", "--port", "0", ...extraArgs], {
      cwd,
      shell: false,
    });

    const output = vscode.window.createOutputChannel("dcode-ai");
    let settled = false;
    let buffered = "";

    const onData = (data) => {
      const text = data.toString();
      buffered += text;
      output.append(text);
      const match = buffered.match(/http:\/\/127\.0\.0\.1:\d+\/\?t=[0-9a-f]+/);
      if (match && !settled) {
        settled = true;
        running = { child, url: match[0] };
        resolve(match[0]);
      }
    };
    child.stdout.on("data", onData);
    child.stderr.on("data", onData);

    child.on("error", (err) => {
      if (!settled) {
        settled = true;
        reject(
          new Error(
            `could not start "${binary}" (${err.message}). Install dcode-ai or set dcode-ai.binaryPath.`
          )
        );
      }
    });
    child.on("exit", (code) => {
      running = null;
      if (!settled) {
        settled = true;
        reject(new Error(`dcode-ai web exited early (code ${code}). See the dcode-ai output channel.`));
      }
    });

    setTimeout(() => {
      if (!settled) {
        settled = true;
        child.kill();
        reject(new Error("timed out waiting for dcode-ai web to print its URL."));
      }
    }, 20000);
  });
}

function deactivate() {
  if (running && running.child.exitCode === null) {
    running.child.kill();
  }
  running = null;
}

module.exports = { activate, deactivate };
