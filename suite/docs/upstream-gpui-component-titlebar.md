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

`#bar` combines `flex_1()` with `flex_shrink_0()`. Disabling shrink means the
bar's flex base size is its **content width**; with no `min_w_0()` its automatic
minimum size is also the content width. Any content wider than the free space —
e.g. a tab strip with many document tabs — overflows the title bar instead of
shrinking, and the trailing `WindowControls` child (itself `flex_shrink_0()`, so
it never yields) is pushed past the window's right edge. The minimize/maximize/
close buttons end up off-screen and unreachable.

## Proposed fix

Let `#bar` shrink and clip:

```rust
.min_w_0()
.overflow_hidden()
```

`flex_1()` then sizes it within the free space and excess content clips (or the
content scrolls/manages its own overflow), while `WindowControls` stays visible.

## How docxy works around it (and what a fix unblocks)

The suite (`suite/docxy/src/main.rs`) does not rely on `#bar`'s flex sizing for
its tab strip. `title_bar_geometry` computes a definite content width up front
(`tabstrip::title_content_w`, accounting for the Root shadow/border, the title
padding, and the caption width), and the strip lays out its chips/arrows/more
button within that width (`tabstrip::layout`, with floor widths and a
more-tabs overflow list). That workaround exists only because `#bar` cannot
shrink; once a pin bump includes the upstream fix it can be simplified to let
the title bar flex normally.
