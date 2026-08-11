# baton

One declarative config for your whole Windows desktop.

Baton doesn't manage windows, draw a bar, or patch the shell. Tools that already
won do that. Baton **conducts** them: you write one file, and it renders every
downstream config, applies the registry settings, and can put all of it back
exactly as it was.

```bash
baton apply       # make the desktop match baton.toml, and reload the WM
baton diff        # show what apply would change, without doing it
baton rollback    # undo the last apply, exactly
```

## Why this exists

Every capability already exists on Windows and is mature: GlazeWM tiles,
Windhawk mods the shell, YASB and zebar draw bars, the registry holds the rest.
What doesn't exist is one place to declare all of it. People hand-assemble
"windots" repos gluing four tools with four config formats, and the result is
bespoke and non-portable.

Baton is the missing layer, not another window manager.

## Status

Early. The config engine and the GlazeWM + registry targets are the v1 scope.

### Targets

| Target | Surface | State |
| --- | --- | --- |
| `glazewm` | `~/.glzr/glazewm/config.yaml` + WebSocket IPC on 6123 | v1 |
| `windows` | Shell and theme settings, mostly `HKCU` | v1 |
| `bar` | Zebar `settings.json`, plus the palette as CSS | v1 |
| `windhawk` | Undocumented `HKLM` store, needs admin | best-effort, later |

The bar target is deliberately narrow. Zebar gets its widgets from marketplace
packs, so what Baton owns is **which widgets launch** — not what they look
like, since pack internals are replaced on every pack update. For theming,
Baton writes your palette to `~/.glzr/zebar/baton-palette.css` as CSS custom
properties, which your own widget pack can import:

```css
@import '../../baton-palette.css';
.bar { background: var(--bg); color: var(--fg); }
```

That is the honest way to get one palette across the desktop without fighting
the marketplace. Baton does not claim to restyle packs it does not own.

Windhawk is deliberately last. It has no orchestration API — its whole CLI is
`-tray-only`, `-exit`, `-restart`, `-safe-mode` — so driving it means writing an
undocumented machine-wide registry format that can change under us. Its
`Portable=1` mode moves that store into files and is the better path if it
works.

## Design

**One config, one palette.** Colours are declared once and referenced as
`"@palette.accent"` from any target, so a theme change propagates everywhere
instead of being retyped in four formats.

**Nothing is applied without a snapshot.** Every value Baton writes is read
first and journaled. `rollback` replays the journal. A crash mid-apply leaves a
journal on disk that the next run finishes undoing.

The journal holds **one** apply, not a history. `rollback` undoes the most
recent `apply` and no further, so applying twice and rolling back once leaves
you at the first apply, not at your original desktop.

**`apply` finishes the job.** It reloads the window manager over its own IPC,
so there is no "now restart it" step. If the WM is not running, that is not an
error: the config is simply ready for when you start it.

**No system files are patched.** Documented config files, the registry, and
each tool's own IPC. Windows Update can't break it.

## Related

`../winrice` is a from-scratch Rust tiling WM built while scoping this. It
works, but GlazeWM does the job better, so winrice is kept only as a
zero-dependency fallback backend.

```bash
cargo test
```
