/**
 * /s/{sheet}/c/{copy}: a copy of a sheet in the editor (a line of its own, compared and merged with the line it came
 * from). `?version=` and `?tab=` work as on the sheet.
 */
import type { Metadata } from "next";
import { SheetPage } from "@/components/sheet/sheet-page";
import { loadLine } from "@/lib/server/calc";

async function loadPage({ params, searchParams }: PageProps<"/s/[sheet]/c/[copy]">) {
  const { sheet, copy } = await params;
  const query = await searchParams;
  const one = (value: string | string[] | undefined) => (Array.isArray(value) ? value[0] : value) ?? null;
  const version = one(query.version);
  const tab = one(query.tab);
  const loaded = await loadLine({ sheet, copy, version: version && /^\d+$/.test(version) ? Number(version) : null, tab: tab && /^\d+$/.test(tab) ? Number(tab) : null });
  return { sheet, copy, loaded };
}

export default async function Page(props: PageProps<"/s/[sheet]/c/[copy]">) {
  const { sheet, copy, loaded } = await loadPage(props);
  return <SheetPage loaded={loaded} sheetId={sheet} copyId={copy} />;
}

export async function generateMetadata(props: PageProps<"/s/[sheet]/c/[copy]">): Promise<Metadata> {
  const { loaded } = await loadPage(props);
  return { title: loaded.kind === "ok" ? loaded.sheet.title : "Copy" };
}
