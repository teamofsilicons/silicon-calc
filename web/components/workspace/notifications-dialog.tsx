"use client";

import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Button } from "@/components/silicon-ui/button/button";
import { Dialog, DialogContent } from "@/components/silicon-ui/dialog/dialog";
import * as calc from "@/lib/calc/api";
import { ApiError } from "@/lib/errors";
import styles from "@/components/sheet/panels/panels.module.css";

export function NotificationsDialog({ accountId, open, onOpenChange }: { accountId: string; open: boolean; onOpenChange: (open: boolean) => void }) {
  const queryClient = useQueryClient();
  const queryKey = ["notifications", accountId];
  const status = useQuery({ queryKey, queryFn: ({ signal }) => calc.getNotifications(signal), enabled: open, retry: false });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const change = async (enabled: boolean) => {
    setBusy(true); setError(null);
    try { queryClient.setQueryData(queryKey, await (enabled ? calc.enableNotifications() : calc.disableNotifications())); }
    catch (failure) { const problem = ApiError.from(failure); setError([problem.message, problem.hint].filter(Boolean).join(" ")); }
    finally { setBusy(false); }
  };
  return <Dialog open={open} onOpenChange={onOpenChange}><DialogContent title="Calc notifications" description="Receive mentions and sharing updates through Silicon Ting.">
    <div className={styles.panel}>
      <p className={styles.secondary}>Enabling notifications lets Calc send your account a Ting when someone mentions you in a comment or shares a sheet or folder with you.</p>
      {status.isPending ? <p role="status">Reading your notification settings…</p> : null}
      {status.isError ? <><p role="alert">{ApiError.from(status.error).message}</p><Button variant="secondary" onClick={() => void status.refetch()}>Try again</Button></> : null}
      {error ? <p role="alert">{error}</p> : null}
      {status.isSuccess ? status.data.enabled ? <><p role="status">Calc notifications are enabled for this account.</p><Button variant="secondary" loading={busy} onClick={() => void change(false)}>Disable Calc notifications</Button></> : <Button loading={busy} onClick={() => void change(true)}>Enable Calc notifications</Button> : null}
    </div>
  </DialogContent></Dialog>;
}
