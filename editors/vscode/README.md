# dcode-ai for VS Code

Embeds the dcode-ai local web chat (`dcode-ai web`) in a VS Code panel.
The extension is a thin launcher — all agent logic stays in the Rust CLI.

## Requirements

- `dcode-ai` v0.0.39+ on PATH (or set `dcode-ai.binaryPath`), with a working
  provider configured (`dcode-ai doctor` to verify).

## Try it without packaging

1. Open `editors/vscode` in VS Code.
2. Press `F5` (Run Extension) — a development host window opens.
3. In the dev host, open your project folder and run the command
   **“dcode-ai: Open Chat”** from the Command Palette.

## Package & install

```bash
npm install -g @vscode/vsce
cd editors/vscode
vsce package          # produces dcode-ai-0.0.1.vsix
code --install-extension dcode-ai-0.0.1.vsix
```

## Settings

| Setting               | Default    | Meaning                                   |
| --------------------- | ---------- | ----------------------------------------- |
| `dcode-ai.binaryPath` | `dcode-ai` | Path to the CLI executable                |
| `dcode-ai.extraArgs`  | `[]`       | Extra args for `dcode-ai web` (e.g. model)|

## How it works

`dcode-ai web --port 0` starts a session plus a loopback HTTP server and
prints a tokenized URL. The extension scrapes that URL from stdout and loads
it in a webview iframe. Closing VS Code kills the server.
