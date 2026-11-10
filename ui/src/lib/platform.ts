/**
 * Window chrome facts the layout needs. No Tauri imports: only what the page can see.
 *
 * `overlayTitlebar`: the macOS window has no title bar of its own (`titleBarStyle: "Overlay"` in
 * tauri.conf.json), so the traffic lights float over our content and AppShell makes room for them.
 * Add `?titlebar` to the URL to preview this in the browser mock.
 */
const inTauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
const isMac = typeof navigator !== 'undefined' && /Mac/.test(navigator.platform) && !/iPhone|iPad/.test(navigator.userAgent);
const forced = typeof location !== 'undefined' && new URLSearchParams(location.search).has('titlebar');

export const overlayTitlebar = (inTauri && isMac) || forced;
