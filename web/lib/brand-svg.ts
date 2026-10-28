/**
 * The app's mark as an SVG document, for the places that need a file rather than a component: the browser icon
 * (app/icon.tsx), the home-screen icon (app/apple-icon.tsx) and the Open Graph image (app/og.png). The glyph is
 * the supplied calc-logo.svg, retaining its original 200 px framing and colours.
 * The system rounds a home-screen icon itself.
 */
import { appConfig } from "./app.config";

export const ICON_PAPER = "#181818";
export const ICON_INK = "#EDE8E0";

const escape = (value: string) => value.replace(/&/g, "&amp;").replace(/"/g, "&quot;").replace(/</g, "&lt;");

/** Preserve the supplied artwork's scale, spacing, and colours for every icon size. */
export function markSvg(): string {
  const paths = appConfig.mark.paths.map(d => `<path d="${escape(d)}"/>`).join("");
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 200 200"><rect width="200" height="200" fill="${ICON_PAPER}"/><g fill="none" stroke="${ICON_INK}" stroke-width="${appConfig.mark.strokeWidth}" stroke-linecap="round" stroke-linejoin="round">${paths}</g></svg>`;
}

export const svgDataUri = (svg: string) => `data:image/svg+xml;base64,${Buffer.from(svg, "utf8").toString("base64")}`;
