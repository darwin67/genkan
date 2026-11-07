# Menu

A macOS-style dropdown from a StatusBar item: the session list or the power actions.

The menu is `menu-width` wide with `menu-padding` inside, `radius-menu`, `menu-material` behind a strong blur, and a `menu-border` hairline. An optional heading uses `type-menu-heading` in `text-menu-heading`. Items are `item-height` tall in `type-menu` with `radius-menu-item`; the highlighted item uses `accent`, and the selected one carries a check mark.

The power menu lists Sleep, Restart… and Shut Down…; the ellipsis marks the two that open an Alert.

**Provide.** The items, the selected item, the highlighted item, and a choose handler.
