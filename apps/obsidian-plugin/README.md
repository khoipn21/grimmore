# Grimmore Obsidian plugin

This desktop-only plugin is Grimmore's UI and the sole authority allowed to change vault Markdown. It starts no companion process during plugin load. Commands attach lazily through the stable `grimmore-launcher` command, which authenticates outside the renderer.

## Development build

From the repository root:

```sh
pnpm install --frozen-lockfile
pnpm --filter @grimmore/protocol build
pnpm --filter @grimmore/obsidian-plugin build
```

Copy `manifest.json`, `main.js`, and `styles.css` into the vault's `.obsidian/plugins/grimmore/` directory. During the Phase 1 development slice, `grimmore-launcher` must be available on the Obsidian process path and the companion must already be serving the same vault ID shown in the plugin settings.

The command palette exposes companion health and an active-note replacement review. A replacement is sent to the companion for revision validation, shown as a complete bounded text diff, and applied only after explicit approval. The expected content revision is checked again inside `Vault.process`; a stale note is not written.

The protected Phase 1 native gate copies this production bundle into a real
temporary vault, starts the native Obsidian desktop application, and verifies
an approved replacement and stale-write refusal through the actual command and
`Vault.process` path. It also records 20 plugin synchronous-load samples from
that live renderer and enforces the 50 ms p95 budget. Stable launcher
installation and cross-platform signing still require their protected native-
runner evidence.
