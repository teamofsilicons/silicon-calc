"use client";

import { useState } from "react";
import { Input } from "@/components/silicon-ui/input/input";
import { Button } from "@/components/silicon-ui/button/button";
import { recordVisit } from "@/lib/calc/api";
import { ApiError } from "@/lib/errors";
import styles from "./editor.module.css";
import sharing from "./sharing.module.css";

export function EmailGate({ sheetId }: { sheetId: string }) {
  const [email, setEmail] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  return <main id="main" className={styles.state}>
    <form className={`${styles.stateCard} ${sharing.gate}`} data-sq="surface" onSubmit={async event => {
      event.preventDefault();
      if (busy) return;
      setBusy(true); setError(undefined);
      try { await recordVisit(sheetId, email); window.location.reload(); }
      catch (failure) { setError(ApiError.from(failure).message); setBusy(false); }
    }}>
      <h1 className={styles.stateTitle}>Enter your email to view</h1>
      <p className={styles.stateText}>The file owner will see your email and when you viewed this file. No sign-in or email verification is needed.</p>
      <Input label="Email" type="email" autoComplete="email" required maxLength={254} value={email} onChange={event => setEmail(event.target.value)} error={error} disabled={busy} />
      <Button type="submit" loading={busy}>View file</Button>
    </form>
  </main>;
}
