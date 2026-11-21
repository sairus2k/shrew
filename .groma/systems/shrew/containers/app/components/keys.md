---
type: C4 Component
title: Keyboard shortcuts
status: stable
groma:
  id: keys
  parent: app
  code:
    - scanner: rust
      file: crates/app/src/keys.rs
  technology: Rust, GPUI
  group: Application shell
description: Binds the keys that drive the app without the mouse and lists them in a help view
---

Defines one action per frequent function (moving through the issue list, opening and closing an issue, search, the status and assignee menus, comment, time, the timer, Open in Jira, sync now, the settings screens) and binds a key to each. The single-key shortcuts are bound in the key context of the issue list, which has the focus while the user types nowhere, so a key typed into an input, a menu or a dialog is never a shortcut. Escape is bound for the whole home screen and leads back to the list.

The views handle the actions: the issue list the ones about the list, the app shell the ones about the open issue, the timer and the other screens. This module also opens the menu of a dropdown button for a shortcut, and shows the help dialog, whose keys are read from the keymap.

Buttons, checkboxes and inputs that have no shortcut are reached with Tab and pressed with Enter or Space; GPUI Kit provides that.
