/**
 * Calc has one look: the calm light theme (styles/tokens.css). There is no dark mode, no theme switch and nothing
 * follows the device's setting. The inline boot script (rendered with the CSP nonce in the root layout's <head>) pins
 * <html data-theme="light"> before the first paint and marks <html data-js>, so controls that only work with script
 * (copy buttons) stay hidden in a browser without it.
 */
export const THEME_BOOT_SCRIPT = `(function(){var r=document.documentElement;r.setAttribute("data-js","");r.setAttribute("data-theme","light");r.style.colorScheme="light"})();`;
