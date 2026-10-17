/**
 * /s/{sheet}: a sheet in the editor. `?version=42` opens that version read-only; `?tab=3` opens that tab first.
 */
import type { Metadata } from "next";
import { SheetPage } from "@/components/sheet/sheet-page";
import { loadLine } from "@/lib/server/calc";

export const metadata: Metadata = { title: "Sheet" };

export default async function Page({ params, searchParams }: PageProps<"/s/[sheet]">) {
  const { sheet } = await params;
  const query = await searchParams;
  const one = (value: string | string[] | undefined) => (Array.isArray(value) ? value[0] : value) ?? null;
  const version = one(query.version);
  const tab = one(query.tab);
  const loaded = await loadLine({ sheet, copy: null, version: version && /^\d+$/.test(version) ? Number(version) : null, tab: tab && /^\d+$/.test(tab) ? Number(tab) : null });
  return <SheetPage loaded={loaded} sheetId={sheet} copyId={null} />;
}

export async function generateMetadata({ params }: PageProps<"/s/[sheet]">): Promise<Metadata> {
  const { sheet } = await params;
  void sheet;
  return { title: "Sheet" };
}
