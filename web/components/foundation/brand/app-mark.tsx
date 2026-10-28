/**
 * The supplied chart mark, in theme-aware ink beside the name like DM's navigation mark. Plain SVG,
 * server-rendered, shared by the landing page, the sign-in page and the signed-in shell.
 *
 *   <AppMark />                 28 px, the bars' size
 *   <AppMark size={44} />       the sign-in card
 *   <Wordmark />                the mark and the app's name
 */
import type { CSSProperties } from "react";
import { appConfig } from "@/lib/app.config";
import styles from "./app-mark.module.css";

export function AppGlyph({ size = 18 }: { size?: number }) {
  return (
    <svg viewBox={appConfig.mark.viewBox} width={size} height={size} fill="none" stroke="currentColor" strokeWidth={appConfig.mark.strokeWidth} strokeLinecap="round" strokeLinejoin="round" aria-hidden="true" focusable="false">
      {appConfig.mark.paths.map(d => <path key={d} d={d} />)}
    </svg>
  );
}

export function AppMark({ size = 28, className }: { size?: number; className?: string }) {
  const style = { "--mark-size": `${size}px` } as CSSProperties;
  return (
    <span className={[styles.mark, className].filter(Boolean).join(" ")} style={style} aria-hidden="true">
      <AppGlyph size={size} />
    </span>
  );
}

/** The wordmark's words ("Silicon Remind"): a link that shows the wordmark is named with them first (WCAG 2.5.3). */
export const BRAND_NAME = appConfig.brandPrefix ? `${appConfig.brandPrefix} ${appConfig.name}` : appConfig.name;

/** The mark and the app's name ("Silicon" and the name, muted, when the config has a brandPrefix), as the bars show it. */
export function Wordmark({ size = 28, className }: { size?: number; className?: string }) {
  return (
    <span className={[styles.wordmark, className].filter(Boolean).join(" ")}>
      <AppMark size={size} />
      <span className={styles.name}>
        {appConfig.brandPrefix ? <>{appConfig.brandPrefix} <span className={styles.muted}>{appConfig.name}</span></> : appConfig.name}
      </span>
    </span>
  );
}
