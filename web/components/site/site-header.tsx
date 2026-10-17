/**
 * The public pages' header (server-rendered), calm like the rest of the Silicon sites: the wordmark on the left and one
 * pill on the right with the docs and Sign in (or "Open Calc" once signed in). Links are plain links, so every public
 * page is a full document.
 */
import { BRAND_NAME, Wordmark } from "@/components/foundation/brand/app-mark";
import { appConfig } from "@/lib/app.config";
import styles from "./site.module.css";

export interface SiteHeaderProps {
  signedIn: boolean;
  /** Where Sign in comes back to (a public sheet asks to come back to itself). */
  returnTo?: string;
}

export function SiteHeader({ signedIn, returnTo }: SiteHeaderProps) {
  const signIn = returnTo ? `/auth/sign-in?return_to=${encodeURIComponent(returnTo)}` : "/auth/sign-in";
  const account = signedIn ? { href: appConfig.home, label: `Open ${appConfig.name}` } : { href: signIn, label: "Sign in" };
  return (
    <header className={styles.header}>
      <a className="skip-link" data-sq="surface" href="#main">Skip to content</a>
      <div className={styles.bar}>
        <a href="/" className={styles.brand} data-sq="surface" aria-label={`${BRAND_NAME}, home`}>
          <Wordmark />
        </a>
        <nav className={styles.pill} data-sq="surface" aria-label="Site">
          <a href={appConfig.links.docs} className={styles.pillLink} data-sq="surface" rel="noopener">Docs</a>
          <a href={account.href} className={`${styles.pillLink} ${styles.pillMain}`} data-sq="surface">{account.label}</a>
        </nav>
      </div>
    </header>
  );
}
