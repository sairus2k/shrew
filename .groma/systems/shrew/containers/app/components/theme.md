---
type: C4 Component
title: Theme
status: stable
groma:
  id: theme
  parent: app
  code:
    - scanner: rust
      file: crates/app/src/theme.rs
  group: Application shell
  technology: Rust, GPUI Kit themes
description: The app's light and dark palette and the small style pieces views share
---

Installs the palette from `theme.json` over the GPUI Kit defaults and follows the system appearance. Also holds the shared pieces of style: the colored dot of a status category, the outlined badge for states such as "Pending", and tabular digits so that changing numbers keep their width.
