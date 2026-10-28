export function shortcutHint(hotkeys, action) {
  return hotkeys?.enabled && hotkeys[action]?.trim() ? ` (${hotkeys[action]})` : "";
}
