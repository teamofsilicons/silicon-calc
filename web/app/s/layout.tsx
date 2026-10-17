/**
 * The sheet editor's frame (/s/…): the same providers as the workspace (query client, toasts, the session keeper),
 * but no sidebar: a sheet takes the whole window, as in any spreadsheet. Signed out is fine here: a public sheet opens
 * without signing in, and the page offers to sign in when it is not public.
 */
import type { ReactNode } from "react";
import { Providers } from "@/components/foundation/providers";
import { getSession } from "@/lib/server/rsc";

export default async function SheetLayout({ children }: { children: ReactNode }) {
  const session = await getSession();
  return (
    <Providers session={session ? { signedIn: true, account: session.account, expiresAt: session.expiresAt } : { signedIn: false, account: null, expiresAt: null }}>
      {children}
    </Providers>
  );
}
