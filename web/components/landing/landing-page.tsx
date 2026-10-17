/**
 * The public landing page (server-rendered, no client code of its own but the copy button): one calm column. What Calc
 * is, Sign in as the one main action, what it does for Carbons and for Silicons as two plain lists, and how a Silicon
 * installs the `calc` command line and signs in. Every word comes from lib/app.config.ts. Semantic HTML: one h1, a
 * section per subject, lists for lists, plain links.
 */
import { Action } from "@/components/site/action";
import { CodeBlock } from "@/components/site/code-block";
import { appConfig, type AppConfig } from "@/lib/app.config";
import styles from "./landing.module.css";

type Feature = AppConfig["landing"]["forCarbons"][number];

function Points({ features }: { features: Feature[] }) {
  return (
    <ul className={styles.points} role="list">
      {features.map(feature => (
        <li key={feature.title}><strong>{feature.title}.</strong> {feature.text}</li>
      ))}
    </ul>
  );
}

export function LandingPage({ signedIn }: { signedIn: boolean }) {
  const { name, appId, landing, links, cli } = appConfig;
  const command = cli?.command ?? appId;
  const siliconCode = [`silicon-apps install ${appId}`, `silicon-accounts login --app ${appId} -q | ${command} login --slt-stdin`, `${command} sheets list`].join("\n");
  const primary = signedIn ? { href: appConfig.home, label: `Open ${name}` } : { href: "/auth/sign-in", label: `Sign in to ${name}` };
  return (
    <div className={styles.page}>
      <header className={styles.hero}>
        <h1 className={styles.title}>{landing.headline}</h1>
        <p className={styles.lede}>{landing.lede}</p>
        <div className={styles.actions}>
          <Action href={primary.href} size="lg">{primary.label}</Action>
          <Action href={links.docs} size="lg" variant="secondary" rel="noopener">Read the docs</Action>
        </div>
        {signedIn ? null : <p className={styles.note}>Sign in with Silicon Accounts. Reading a public sheet never needs signing in.</p>}
      </header>

      <section className={styles.section} id="for-carbons" aria-labelledby="carbons-title">
        <h2 id="carbons-title" className={styles.heading}>For Carbons</h2>
        <p className={styles.text}>
          Bring in your data, comment and make changes in the browser. It works the way Google Sheets and Excel do, and
          everyone sees every change as it happens.
        </p>
        <Points features={landing.forCarbons} />
      </section>

      <section className={styles.section} id="for-silicons" aria-labelledby="silicons-title">
        <h2 id="silicons-title" className={styles.heading}>For Silicons</h2>
        <p className={styles.text}>
          The <code>{command}</code> command line does everything the web app can. Install it through Silicon Apps, which
          keeps it up to date, and sign in with a short-lived token from Silicon Accounts.
        </p>
        <CodeBlock code={siliconCode} lang="sh" title="Install and sign in" />
        <Points features={landing.forSilicons} />
      </section>

      <p className={styles.closing}>
        Find {name} <a href={links.store} rel="noopener">on Silicon Apps</a>, read <a href={links.docs} rel="noopener">the docs</a>, or
        read <a href="/llms.txt">llms.txt</a>.
      </p>
    </div>
  );
}
