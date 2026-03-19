---
name: slint-gui
description: |
    Use when creating, refactoring, or debugging Slint UI in sven-gui. Covers .slint markup,
    Theme usage, Rust bindings, callbacks, and sven-specific layout patterns. Load for tasks
    involving crates/sven-gui, .slint files, Slint components, or sven desktop UI changes.
---

# Slint GUI — sven-gui

## Location and structure

Slint UI lives in `crates/sven-gui/`:

```
crates/sven-gui/
├── build.rs              # Compiles ui/main-window.slint
├── src/
│   └── lib.rs            # Rust bridge: instantiates MainWindow, wires callbacks
└── ui/
    ├── main-window.slint # Root window, imports all components
    ├── theme.slint       # Design tokens (colors, spacing, fonts)
    ├── widgets.slint     # Reusable widgets (IconButton, SearchInput, …)
    ├── sidebar.slint     # Session list
    ├── chat-pane.slint   # Message list
    ├── input-pane.slint  # Input area
    ├── status-bar.slint  # Bottom bar
    ├── toast.slint       # Toast notifications
    ├── question-modal.slint
    ├── picker-popup.slint
    ├── completion-popup.slint
    ├── queue-panel.slint
    ├── search-bar.slint
    ├── help-overlay.slint
    ├── pager.slint
    ├── team-picker.slint
    └── inspector.slint
```

Build entry point: `build.rs` compiles `ui/main-window.slint`. All other `.slint` files are imported from there.

## Theme usage

All UI must use `Theme` from `theme.slint` for colors, spacing, typography, and animation durations.

```slint
import { Theme } from "theme.slint";

Rectangle {
    background: Theme.bg-surface;
    color: Theme.text-primary;
    font-size: Theme.font-size-sm;
    padding: Theme.space-sm;
    border-radius: Theme.radius-md;
    animate background { duration: Theme.duration-fast; }
}
```

Common tokens:
- **Backgrounds**: `Theme.bg-deep`, `Theme.bg-surface`, `Theme.bg-card`, `Theme.bg-hover`, `Theme.bg-active`
- **Text**: `Theme.text-primary`, `Theme.text-secondary`, `Theme.text-dim`
- **Accent**: `Theme.accent`, `Theme.accent-dim`
- **Semantic**: `Theme.success`, `Theme.error-text`, `Theme.warning`
- **Spacing**: `Theme.space-xs` … `Theme.space-2xl`
- **Layout**: `Theme.sidebar-width`, `Theme.row-height`, `Theme.status-bar-height`

## Data model: structs and properties

Components receive data via `in property` and expose state via `in-out property`. Use structs for complex items:

```slint
export struct SessionItem {
    id: string,
    title: string,
    busy: bool,
    active: bool,
    depth: int,
    status: string,
    current-tool: string,
    total-cost-usd: float,
}

component SessionRow inherits Rectangle {
    in property <SessionItem> item;
    in property <int> index;
    callback clicked(int);
    callback delete-clicked(string);
    // ...
}
```

## Callbacks

Wire callbacks from Rust to Slint:

```slint
export component Sidebar inherits Rectangle {
    in property <[SessionItem]> sessions;
    callback session-selected(string);
    callback new-session();
    callback session-delete-requested(string);
    // ...
}
```

In Rust: `slint::include_modules!();` then `app.on_session_selected(|id| { ... });`.

## Layout patterns

- **HorizontalLayout / VerticalLayout**: Use for toolbars, forms, lists. Set `spacing`, `alignment`, `padding`.
- **Flickable**: Wrap scrollable content. Set `viewport-height` to the list’s `preferred-height`.
- **Conditional**: `if item.active: Rectangle { ... }` for state-dependent UI.
- **Stretch**: `vertical-stretch: 1` or `horizontal-stretch: 1` to fill parent.

## Animations

Use `animate` blocks for transitions:

```slint
background: ta.has-hover ? Theme.bg-hover : transparent;
animate background { duration: Theme.duration-fast; }
```

Opacity: `animate opacity { duration: Theme.duration-fast; }` for hover/visibility.

## Hover and interaction

Use `TouchArea` for click/tap:

```slint
ta := TouchArea {
    clicked => { root.clicked(root.index); }
}
```

Check hover state: `ta.has-hover`, `parent.has-hover`.

## Repeaters and lists

```slint
for item[i] in root.sessions: SessionRow {
    item: item;
    index: i;
    row-width: root.width;
    clicked(idx) => { root.session-selected(root.sessions[idx].id); }
}
```

## Custom widgets

Reusable widgets live in `widgets.slint` (e.g. `IconButton`, `SearchInput`). Import and use:

```slint
import { IconButton, SearchInput } from "widgets.slint";
```

## Rust integration

1. `build.rs` compiles `ui/main-window.slint`.
2. `slint::include_modules!();` to load generated code.
3. `MainWindow::new()?` to create the window.
4. Set properties: `app.set_property_name(value);`
5. Set callbacks: `app.on_callback_name(|args| { ... });`
6. `app.run()` to start the event loop.

## Build and run

```bash
# From workspace root
cargo build -p sven-gui
cargo run -p sven-gui

# With preview
SLINT_STYLE=material-light cargo run -p sven-gui
```

## Debug

- `debug(value)` in `.slint` prints to stderr.
- `SLINT_SLOW_ANIMATIONS=4` to slow animations.
- `SLINT_DEBUG_PERFORMANCE=overlay` for FPS overlay.

## Checklist for new UI

- [ ] Use `Theme` for all colors, spacing, fonts.
- [ ] Export structs and components needed by other modules.
- [ ] Use callbacks for user actions; wire in Rust.
- [ ] Add `animate` for hover/state transitions.
- [ ] Import from `main-window.slint` if the new component is used by MainWindow.
- [ ] Run `cargo build -p sven-gui` to verify.

## Additional resources

- For full Slint concepts (layouts, gradients, themes, advanced patterns), see [.cursor/skills/slint/SKILL.md](/data/.cursor/skills/slint/SKILL.md).
- For sven repo layout, see [.agents/skills/repo-structure/SKILL.md](../repo-structure/SKILL.md).
