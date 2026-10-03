# Markdown Editor: Build Spec

App name: Just a markdown editor.

## 1. Goal

A minimalist, extremely lightweight markdown editor and note-taking app with inline live preview. Speed is the primary requirement: the app must feel instant regardless of file size. Windows first, with a macOS port kept viable. Every dependency and abstraction has to earn its place.

The coding agent works with Ponytail. The module boundaries in section 13 and the sync provisions in section 14 are explicit requirements and must be kept even where a flatter design would be shorter.

## 2. Platform and stack

- Rust (stable). Windows 10/11 first.
- Custom-drawn interface. No webview and no heavyweight GUI framework.
- Suggested crates: `winit` (window, input), `cosmic-text` (shaping, layout, font fallback), a rope crate for the buffer, `pulldown-cmark` for inline parsing, `notify` for file watching. Substitute any of these if there is a measured reason.
- The renderer sits behind an interface. Start with CPU rendering, damage tracking and a glyph cache. Add a GPU backend only if the frame budget is missed on high-resolution displays.
- Platform-specific code lives in one module so the Mac port touches nothing else. On macOS, Cmd replaces Ctrl in all shortcuts.

## 3. Layout

```
+---------------------------------------------------------------+
| native title bar                                              |
+---------+-----------------------------------------+-----------+
| folder  | tabs                                    | outline   |
| tree    +-----------------------------------------+ (headings)|
|         | toolbar                                 |           |
|         +-----------------------------------------+           |
|         | editor                                  |           |
+---------+-----------------------------------------+-----------+
```

- Native title bar, tabs beneath it.
- Left: folder tree, resizable and collapsible.
- Right: heading outline, resizable and collapsible.
- Center: one or more panes, each with its own tab strip.
- Toolbar: slim, at the top of the editing area, acting on the active pane. Buttons: heading selector, bold, italic, strikethrough, list, code block. Nothing else.

## 4. Editor

### Live preview

- Single pane. Markdown is styled in place as you type.
- Default: syntax markers are hidden everywhere except on the line the cursor is on, where the raw source is shown.
- Setting "show markers": markers stay visible on all lines, dimmed.

### Rendered in v1

Headings, bold, italic, strikethrough, bullet and numbered lists, task checkboxes (clickable), links, block quotes, inline code, fenced code blocks, horizontal rules.

- Code blocks: monospace font on a tinted background. No per-language coloring.
- Tables: shown as tidy monospace source, not rendered.

### Editing behavior

- Soft wrap, with an optional maximum line width.
- Undo and redo, stored as an operation log.
- Find and replace in the current file.
- List continuation on Enter, indent and outdent of list items, bracket and quote pairing.
- Input method support for composed input (Japanese, Chinese, Korean). Right-to-left scripts are not a v1 target.
- UTF-8. Preserve the file's existing line endings.

## 5. Tabs and splits

- Multiple files open as tabs. Tabs can be reordered and show an unsaved marker when relevant.
- Panes form a split tree: any pane can be split right or down, and each leaf holds its own tabs.
- Split by command first. Then add the gesture: dragging a tab to a pane edge shows a drop zone and splits there; dragging to another pane's tab strip moves the tab.
- A pane closes when its last tab closes.

## 6. Side panels

### Folder tree

- One workspace folder per window. Shows folders and markdown files.
- Click opens a file. Create, rename, and delete (to the recycle bin) from the tree.
- Loads lazily and updates when files change on disk.

### Outline

- Lists the headings of the document in the active pane, indented by level, with collapsible sub-levels.
- Highlights the section the cursor is in. Click jumps to the heading.
- Updates live as headings change.

## 7. Commands and navigation

- Every action is a named command in one registry. Toolbar buttons, shortcuts, and the command palette all dispatch through it.
- Command palette: fuzzy search over all commands.
- Quick open: fuzzy search over file names in the workspace.
- Jump to heading: fuzzy search over the current document's headings.
- Folder search: text search across the workspace, results open at the match.

## 8. Focus mode

- One shortcut toggles it. Everything is hidden except the text of the active pane: panels, tabs, toolbar, and the title bar (borderless fullscreen).
- The same shortcut or Esc exits.
- Optional settings: typewriter scrolling (cursor line stays vertically centered) and maximum line width.

## 9. Default shortcuts

All are remappable.

| Action | Shortcut |
|---|---|
| Bold / italic | Ctrl+B / Ctrl+I |
| Strikethrough | Alt+Shift+5 |
| Heading level 1 to 6 | Ctrl+1 to Ctrl+6 |
| Bullet / numbered list | Ctrl+Shift+8 / Ctrl+Shift+7 |
| Inline code / code block | Ctrl+E / Ctrl+Shift+K |
| Link | Ctrl+K |
| Save | Ctrl+S |
| Undo / redo | Ctrl+Z / Ctrl+Y |
| Find / replace / folder search | Ctrl+F / Ctrl+H / Ctrl+Shift+F |
| Quick open / command palette | Ctrl+P / Ctrl+Shift+P |
| Jump to heading | Ctrl+J |
| New file / open folder | Ctrl+N / Ctrl+O |
| Close tab / reopen closed tab | Ctrl+W / Ctrl+Shift+T |
| Next / previous tab | Ctrl+Tab / Ctrl+Shift+Tab |
| Split right / split down | Ctrl+\ / Ctrl+Shift+\ |
| Toggle folder tree / outline | Ctrl+Shift+E / Ctrl+Shift+O |
| Focus mode | F11 |
| Settings | Ctrl+, |

## 10. Settings and theming

- One human-editable config file is the source of truth. The settings screen reads and writes it.
- Settings screen: theme (light or dark), accent color, font, show markers, maximum line width, typewriter scrolling, autosave. A button opens a separate keymap subsection listing every command with its shortcut.
- Themes are a small set of color tokens. The dark theme uses warm tones (browns, warm grays, off-white text), not blue-tinted grays. The accent color is one token the user can override.
- Font selector for the editor and interface, plus a monospace font for code.

## 11. Files and saving

- Plain markdown files on disk are the only source of truth. No database, no proprietary format.
- Autosave shortly after typing stops. Ctrl+S saves immediately.
- Saves are atomic (write to a temporary file, then replace).
- External changes are detected. A file with no unsaved edits reloads silently; otherwise the user chooses which version to keep.
- Session restore: reopening the app restores the workspace, open tabs, split layout, and cursor positions. Session state is stored separately from settings.

## 12. Performance requirements

These are requirements, not aspirations. They must hold on a 1 MB file, a 100 MB file, and a 10 MB file consisting of a single line.

- Keystroke to updated frame: under 5 ms of processing.
- Scrolling: every frame within the display's frame budget.
- Cold start to a usable window: under 100 ms.
- Idle: 0% CPU. No continuous redraw loop.
- Opening a large file shows the first screen immediately while the rest loads.

Memory may scale with file size; per-keystroke and per-frame cost may not.

Expected techniques:

- Rope buffer with cheap snapshots.
- Layout and shaping only for visible lines plus a small margin, cached per line.
- Estimated heights for lines not yet laid out, refined lazily, so the scrollbar never requires laying out the whole file.
- Two-tier parsing: an incremental line scanner tracks block state for the whole document; full inline parsing runs only on visible blocks.
- Very long lines are wrapped and shaped in chunks.
- Background threads for file loading, the initial scan, folder indexing and search, and saving. The interface thread never waits on disk or on whole-file work.
- Fonts: load the configured font first; enumerate system fonts lazily.
- Release build tuned for speed and size.

## 13. Architecture

```
platform/   window, input, clipboard, input methods, fonts, file watching
render/     renderer interface, CPU backend, glyph cache, damage tracking
core/
  buffer    rope, edits, undo log
  markdown  block scanner, inline parser, style ranges, heading index
  commands  command registry and keymap
  storage   storage interface and local-disk implementation
  workspace folder index, fuzzy matching, search
  config    settings, themes, session state
ui/         split tree, panes, tabs, editor view, panels, toolbar, overlays
```

- `core` has no dependency on `ui`, `render`, or `platform`.
- `ui` talks to files only through the storage interface.

## 14. Cloud sync readiness

Sync is not built in v1, but the design must allow adding it without rework:

- All file access goes through the storage interface (read, write, list, watch). Local disk is the first implementation; a sync provider can be added as another.
- Core code uses workspace-relative paths, not absolute ones.
- External-change detection and the keep-which-version flow already exist and are the hook for sync conflicts.
- The undo operation log and atomic saves stay as specified.
- Device-specific state (session, window layout) is kept apart from portable state (settings, keymap, themes).

## 15. Not in v1

Leave room for these, but do not build them: rendered tables, inline images, math, wiki-links, export, spellcheck, per-language code coloring, right-to-left text, screen reader support, dragging files between folders in the tree, multiple workspace folders per window.

## 16. Build order

1. Editor surface: open one file, live preview, typing, selection, undo, wrap, composed input, all meeting section 12. Do not proceed until it does.
2. Tabs, saving and autosave, folder tree.
3. Command registry, shortcuts, toolbar, command palette, quick open.
4. Split commands, outline panel, jump to heading, find and replace, folder search.
5. Focus mode, themes, config file, session restore.
6. Settings screen and keymap subsection.
7. Drag-to-split gesture.

## 17. Open items

- App icon.
- Distribution: portable executable or installer.
