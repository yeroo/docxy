# Upstream report draft: gpui-component `TitleBar` `#bar` can push `WindowControls` off-screen

Ready-to-file report for <https://github.com/longbridge/gpui-component>. Filing it
is an outward-facing act on a third-party repo and is left to a maintainer; this
file is the in-repo record (#545, leftover from #541 / PR #544).

## Where

- Repo: `longbridge/gpui-component`, pinned rev `bc174a7ec4534b2a4174fddde314b38d30d69093`
  (`suite/Cargo.toml`).
- File: `crates/ui/src/title_bar.rs`, the `#bar` child of the title bar, lines
  ~302-307 in the pinned rev:

```rust
h_flex()
    .id("bar")
    .h_full()
    .justify_between()
    .flex_shrink_0()
    .flex_1()
    ...
    .children(self.children),
```

## Defect

`#bar` is declared `.flex_shrink_0().flex_1()`, but in gpui `flex_1()` sets
grow 1, shrink 1, basis 0% — so the earlier `flex_shrink_0()` is dead and the
bar can in principle shrink. What it cannot do is shrink below its
**automatic minimum size**: with no `min_w_0()`, the flex item's min-width
resolves to its content's min-content width. Any content wider than the free
space — e.g. a tab strip with many document tabs — makes `#bar` overflow the
title bar instead of shrinking, and the trailing `WindowControls` child
(itself `flex_shrink_0()`, so it never yields) is pushed past the window's
right edge. The minimize/maximize/close buttons end up off-screen and
unreachable.

## Proposed fix

Give `#bar` a zero automatic minimum and let excess content clip:

```rust
.min_w_0()
.overflow_hidden()
```

`flex_1()` then sizes it within the free space and `WindowControls` stays
visible. Removing the dead `flex_shrink_0()` from the declaration is cleanup
the same change can carry.

## How docxy works around it (and what a fix unblocks)

The suite (`suite/docxy/src/main.rs`) does not rely on `#bar`'s flex sizing for
its tab strip. `title_bar_geometry` computes a definite content width up front
(`tabstrip::title_content_w`, accounting for the Root shadow/border, the title
padding, and the caption width), and the strip lays out its chips/arrows/more
button within that width (`tabstrip::layout`, with floor widths and a
more-tabs overflow list). That workaround exists only because `#bar` cannot
shrink; once a pin bump includes the upstream fix it can be simplified to let
the title bar flex normally.
