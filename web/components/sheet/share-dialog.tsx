"use client";

import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Dialog, DialogContent } from "@/components/silicon-ui/dialog/dialog";
import { Button } from "@/components/silicon-ui/button/button";
import { Checkbox } from "@/components/silicon-ui/checkbox/checkbox";
import { Input } from "@/components/silicon-ui/input/input";
import { Select } from "@/components/silicon-ui/select/select";
import { HoldToConfirm } from "@/components/silicon-ui/hold-to-confirm/hold-to-confirm";
import * as calc from "@/lib/calc/api";
import type { PublicAccess, Role } from "@/lib/calc/types";
import { ApiError } from "@/lib/errors";
import { notify, notifyError } from "@/lib/notify";
import sharingStyles from "./sharing.module.css";
import styles from "./panels/panels.module.css";

const roles = [{ value: "editor", label: "Editor" }, { value: "commenter", label: "Commenter" }, { value: "viewer", label: "Viewer" }];

export function ShareDialog({ item, title, open, onOpenChange, canManage = true }: { item: calc.ItemRef; title: string; open: boolean; onOpenChange: (open: boolean) => void; canManage?: boolean }) {
  const [invite, setInvite] = useState("");
  const [role, setRole] = useState<Exclude<Role, "owner">>("editor");
  const [busy, setBusy] = useState(false);
  const [pendingEmail, setPendingEmail] = useState<boolean | null>(null);
  const sharing = useQuery({ queryKey: ["sharing", item.kind, item.id], queryFn: ({ signal }) => calc.getSharing(item, signal), enabled: open });
  const change = async (action: () => Promise<unknown>) => {
    setBusy(true);
    try { await action(); await sharing.refetch(); } catch (error) { notifyError(error); } finally { setBusy(false); }
  };
  return <Dialog open={open} onOpenChange={onOpenChange}><DialogContent className={sharingStyles.dialog} title={`Share ${title}`} description="Invite Carbons and Silicons by their c: or si: id, or email.">
    <div className={`${styles.panel} ${sharingStyles.section}`}>
      {sharing.isPending ? <p role="status">Reading sharing…</p> : null}
      {sharing.isError ? <p role="alert">{ApiError.from(sharing.error).message} <Button variant="ghost" size="sm" onClick={() => void sharing.refetch()}>Try again</Button></p> : null}
      {canManage ? <form className={`${styles.panel} ${sharingStyles.section}`} onSubmit={event => { event.preventDefault(); void change(async () => { await calc.invite(item, { invite: invite.split(/[,\s]+/).filter(Boolean), role, notify: true }); setInvite(""); notify.success("Invitation sent"); }); }}>
        <Input label="Ids or emails" placeholder="si:analyst, c:ada" value={invite} onChange={event => setInvite(event.target.value)} />
        <Select label="Access" options={roles} value={role} onValueChange={value => setRole(value as Exclude<Role, "owner">)} />
        <Button type="submit" loading={busy} disabled={!invite.trim()}>Invite</Button>
      </form> : null}
      {sharing.data ? <>
        <p>{sharing.data.owner.display_name} · {sharing.data.owner.id} · Owner</p>
        <ul className={styles.list}>{sharing.data.members.map(member => <li className={styles.item} key={member.account.uuid}>
          <span>{member.account.display_name} · {member.account.id}</span>
          {canManage && member.via === "direct" ? <><Select label={`Access for ${member.account.id}`} value={member.role} options={roles} onValueChange={value => void change(() => calc.changeRole(item, member.account.uuid, value as Exclude<Role, "owner">))} /><HoldToConfirm tone="danger" label={`Hold to remove ${member.account.id}`} onConfirm={() => void change(() => calc.removeMember(item, member.account.uuid))} /></> : <span className={styles.muted}>{member.role}{member.via !== "direct" ? ` · via ${member.via.folder.name}` : ""}</span>}
        </li>)}</ul>
        {sharing.data.invites.map(pending => <div key={pending.email} className={styles.item}><span>{pending.email} · {pending.role} · invited</span>{canManage ? <HoldToConfirm label="Hold to cancel invitation" tone="danger" onConfirm={() => void change(() => calc.removeInvite(item, pending.email))} /> : null}</div>)}
        {item.kind === "sheet" ? <section className={`${styles.panel} ${sharingStyles.section}`} aria-label="Public link">
          <h2>Public link</h2>
          <p className={styles.secondary}>Share a public link to this file. Commenting and editing require sign-in.</p>
          {canManage ? <Select label="Link access" value={sharing.data.public.enabled ? sharing.data.public.access : "off"} options={[{ value: "off", label: "Private" }, { value: "view", label: "Anyone can view" }, { value: "comment", label: "Signed-in accounts can comment" }, { value: "edit", label: "Signed-in accounts can edit" }]} onValueChange={value => void change(() => calc.setPublic(item.id, { enabled: value !== "off", access: (value === "off" ? "view" : value) as PublicAccess }))} /> : null}
          {sharing.data.role === "owner" && sharing.data.public.enabled ? <>
            <Checkbox label="Require email to view" checked={pendingEmail ?? sharing.data.public.require_email ?? false} disabled={busy} onCheckedChange={checked => { setPendingEmail(checked === true); void change(() => calc.setPublic(item.id, { require_email: checked === true })).finally(() => setPendingEmail(null)); }} />
            <p className={styles.secondary}>Use the signed-in account’s email when available. Other viewers enter an email without signing in or verifying it.</p>
          </> : null}
          <Button variant="secondary" onClick={() => void navigator.clipboard.writeText(`${location.origin}/s/${item.id}`).then(() => notify.success("Link copied"), error => notifyError(error))}>Copy link</Button>
          {sharing.data.role === "owner" ? <ViewerHistory sheetId={item.id} collecting={sharing.data.public.enabled && !!sharing.data.public.require_email} /> : null}
        </section> : null}
      </> : null}
    </div>
  </DialogContent></Dialog>;
}

function ViewerHistory({ sheetId, collecting }: { sheetId: string; collecting: boolean }) {
  const [expanded, setExpanded] = useState(false);
  const [offset, setOffset] = useState(0);
  const viewers = useQuery({ queryKey: ["file-viewers", sheetId, offset], queryFn: ({ signal }) => calc.getViewers(sheetId, offset, signal), enabled: expanded });
  const date = (value: string) => new Date(value).toLocaleString();
  return <details className={sharingStyles.history} onToggle={event => setExpanded(event.currentTarget.open)}>
    <summary>Viewers{viewers.data ? ` (${viewers.data.total})` : ""}</summary>
    <div className={sharingStyles.historyBody}>
      <p className={styles.secondary}>Only you can see this history. {collecting ? "Views are recorded while the email requirement is enabled." : "Enable the email requirement to record new viewers."}</p>
      <Button size="sm" variant="ghost" loading={viewers.isFetching} onClick={() => void viewers.refetch()}>Refresh viewers</Button>
      {viewers.isPending ? <p role="status">Reading viewers…</p> : null}
      {viewers.isError ? <p role="alert">{ApiError.from(viewers.error).message}</p> : null}
      {viewers.data?.total === 0 ? <p className={styles.secondary}>No viewers yet.</p> : null}
      <ul>{viewers.data?.viewers.map(viewer => <li key={`${viewer.email_source}:${viewer.email}`} className={sharingStyles.viewer}>
        <span className={sharingStyles.email}>{viewer.email}</span>
        <span className={sharingStyles.detail}>{viewer.email_source === "account" ? "Account email" : "Entered email · unverified"}</span>
        <span className={sharingStyles.detail}>First viewed {date(viewer.first_viewed_at)}<br />Last viewed {date(viewer.last_viewed_at)}</span>
      </li>)}</ul>
      {offset > 0 || viewers.data?.next_offset !== null && viewers.data?.next_offset !== undefined ? <div className={sharingStyles.pager}>
        <Button size="sm" variant="secondary" disabled={offset === 0} onClick={() => setOffset(Math.max(0, offset - 50))}>Previous</Button>
        <span className={sharingStyles.detail}>{offset + 1}–{offset + (viewers.data?.viewers.length ?? 0)}</span>
        <Button size="sm" variant="secondary" disabled={viewers.data?.next_offset == null} onClick={() => setOffset(viewers.data?.next_offset ?? offset)}>Next</Button>
      </div> : null}
    </div>
  </details>;
}
