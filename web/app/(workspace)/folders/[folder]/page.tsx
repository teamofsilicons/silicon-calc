import { SheetsPage } from "@/components/workspace/sheets-page";
export const metadata = { title: "Folder" };
export default async function Page({ params }: PageProps<"/folders/[folder]">) { const { folder } = await params; return <SheetsPage key={folder} folderId={folder} />; }
