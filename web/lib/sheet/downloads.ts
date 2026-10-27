import type { DownloadFormat } from "../calc/types";

export const DOWNLOAD_FORMATS: ReadonlyArray<{ format: DownloadFormat; label: string }> = [
  { format: "xlsx", label: "Microsoft Excel (.xlsx)" },
  { format: "ods", label: "OpenDocument (.ods)" },
  { format: "pdf", label: "PDF (.pdf)" },
  { format: "html", label: "Web page (.html)" },
  { format: "csv", label: "Comma-separated values (.csv)" },
  { format: "tsv", label: "Tab-separated values (.tsv)" },
];
export const EXTRA_DOWNLOAD_FORMATS: typeof DOWNLOAD_FORMATS = [
  { format: "xls", label: "Excel 97–2003 (.xls)" },
  { format: "numbers", label: "Apple Numbers (.numbers)" },
  { format: "json", label: "JSON (.json)" },
  { format: "parquet", label: "Apache Parquet (.parquet)" },
];
export function downloadOptions(format: DownloadFormat, activeTab: number, version: number | null) {
  return { tab: ["csv", "tsv", "parquet"].includes(format) ? activeTab : undefined, version };
}
