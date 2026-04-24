# CLI terminal fonts

`dcode-ai` interactive mode draws a full-screen TUI (Ratatui + crossterm). Everything is plain Unicode in a **fixed character grid**. The Rust process **cannot** set the typeface—only your **terminal emulator** chooses the font.

## Plus Jakarta Sans

**Plus Jakarta Sans** is a **proportional** sans. If you set it as the terminal font, borders, tables, and layouts in the TUI will not line up. For `dcode-ai`’s full-screen UI, use a **monospace** font.

You can still use Jakarta for the rest of the IDE (tabs, UI); keep the **integrated terminal** on monospace.

## Fonts with a similar “modern” feel to many coding agents

These are common monospace choices that read close to products like Claude Code / modern dev UIs:

| Font | Notes |
|------|--------|
| [Geist Mono](https://vercel.com/font) | Clean, contemporary |
| [JetBrains Mono](https://www.jetbrains.com/lp/mono/) | Very popular for terminals |
| SF Mono | macOS (with Xcode / system) |
| [Cascadia Code](https://github.com/microsoft/cascadia-code) | Microsoft, ligatures optional |

Install the font in the OS, then point your terminal at it.

## Cursor / VS Code

**Settings → search “terminal font”** or edit `settings.json`:

```json
"terminal.integrated.fontFamily": "'JetBrains Mono', 'Geist Mono', 'SF Mono', monospace",
"terminal.integrated.fontSize": 13
```

## iTerm2 / Terminal.app / WezTerm / Ghostty

Use each app’s profile or config to set the **monospace** font for the profile you use with `dcode-ai`.

## Line REPL (`dcode-ai --no-tui`)

Same rule: output is still terminal text; font remains an emulator setting.
