import { readFile } from "node:fs/promises";
import { expect, test, type Page } from "@playwright/test";
import { accountsSession, runAccounts, sameOrigin, signIn, watchErrors } from "./support";
import { DOWNLOAD_FORMATS } from "../lib/sheet/downloads";

async function menu(page: Page) {
  await page.getByRole("button", { name: "Sheet actions", exact: true }).click();
  await page.getByRole("menuitem", { name: "Download", exact: true }).hover();
  await expect(page.getByRole("menuitem", { name: "Microsoft Excel (.xlsx)", exact: true })).toBeVisible();
}

test("downloads use portable results, preserve workbook tabs and restore runnable Calc code", async ({ browser }) => {
  test.setTimeout(180_000);
  const context = await accountsSession(browser, "carbon", { reducedMotion: "reduce" });
  const page = await context.newPage();
  const errors = watchErrors(page);
  try {
    await signIn(page, runAccounts().carbon.email);
    await page.getByRole("button", { name: "New sheet", exact: true }).click();
    await expect(page.getByTestId("grid")).toBeVisible();
    const id = new URL(page.url()).pathname.split("/")[2]!;
    const base = `/api/v1/sheets/${id}`;
    const save = await page.request.put(`${base}/functions`, { headers: sameOrigin(), data: { base_version: 0, function: { name: "EXPORT_DOUBLE", parameters: ["v"], code: "return v * 2;" } } });
    expect(save.ok()).toBeTruthy();
    const seed = await page.request.post(`${base}/edits`, { headers: sameOrigin(), data: { edits: [{ type: "set", tab: 1, at: "A1", rows: [["5", "=EXPORT_DOUBLE(A1)", "=B1+1", '=JS("return [[1,2],[3,4]];")', "", "=SUM(D1#)"]] }] } });
    expect(seed.ok()).toBeTruthy();
    const values = async (sheet: string, tab = 1, range = "A1:F2") => (await (await page.request.get(`/api/v1/sheets/${sheet}/tabs/${tab}/cells?range=${range}&format=values`)).json()).values;
    await expect.poll(() => values(id)).toEqual([[5, 10, 11, 1, 2, 10], [null, null, null, 3, 4, null]]);
    const state = await (await page.request.get(`${base}/functions`)).json();
    const command = 'return { columns: ["Input", "Double"], rows: [[await calc.cell(1, "A1"), await calc.cell(1, "B1")]] };';
    const run = await page.request.post(`${base}/run`, { headers: sameOrigin(), data: { base_version: state.version, code: command, inputs: [1], columns: ["Input", "Double"], rows: [[5, 10]], into: { new_tab: "Analysis" } } });
    expect(run.ok()).toBeTruthy();
    await page.getByRole("tab", { name: "Analysis", exact: true }).click();
    await expect(page.getByRole("tab", { name: "Analysis", exact: true })).toHaveAttribute("aria-selected", "true");
    await expect.poll(() => values(id)).toEqual([[5, 10, 11, 1, 2, 10], [null, null, null, 3, 4, null]]);

    const files = new Map<string, Buffer>();
    for (const { format, label } of DOWNLOAD_FORMATS) {
      await menu(page);
      if (format === "xlsx") {
        for (const entry of DOWNLOAD_FORMATS) await expect(page.getByRole("menuitem", { name: entry.label, exact: true })).toBeVisible();
        await expect(page.getByRole("menu").filter({ has: page.getByRole("menuitem", { name: "Microsoft Excel (.xlsx)", exact: true }) })).toHaveCSS("opacity", "1");
        await page.screenshot({ path: test.info().outputPath("download-menu.png"), animations: "disabled" });
      }
      const pending = page.waitForEvent("download");
      await page.getByRole("menuitem", { name: label, exact: true }).click();
      const download = await pending;
      expect(await download.failure()).toBeNull();
      expect(download.suggestedFilename()).toMatch(new RegExp(`\\.${format}$`));
      const url = new URL(download.url());
      expect(url.searchParams.get("tab")).toBe(["csv", "tsv"].includes(format) ? "2" : null);
      files.set(format, await readFile((await download.path())!));
    }
    expect(files.get("csv")!.toString()).toBe("Input,Double\n5,10\n");
    expect(files.get("tsv")!.toString()).toBe("Input\tDouble\n5\t10\n");
    expect(files.get("html")!.toString()).toContain("Analysis");
    expect(files.get("html")!.toString()).toContain("<td class=\"number\" style=\"\">11</td>");
    expect(files.get("pdf")!.subarray(0, 5).toString()).toBe("%PDF-");

    for (const format of ["xlsx", "ods"]) {
      const response = await page.request.post(`/api/v1/sheets/import?filename=restored.${format}`, { headers: sameOrigin({ "Content-Type": "application/octet-stream" }), data: files.get(format)! });
      const restored = await response.json();
      expect(response.ok(), JSON.stringify(restored)).toBeTruthy();
      expect(restored.warnings ?? []).toEqual([]);
      expect(restored.tabs).toHaveLength(2);
      expect(restored.tabs[1].code.code).toBe(command);
      await page.goto(`/s/${restored.id}`);
      await expect.poll(() => values(restored.id)).toEqual([[5, 10, 11, 1, 2, 10], [null, null, null, 3, 4, null]]);
      await page.request.post(`/api/v1/sheets/${restored.id}/edits`, { headers: sameOrigin(), data: { edits: [{ type: "set", tab: 1, at: "A1", rows: [["7"]] }] } });
      await expect.poll(() => values(restored.id, 1, "A1:C1")).toEqual([[7, 14, 15]]);
      await page.getByRole("tab", { name: "Analysis", exact: true }).click();
      await page.getByRole("button", { name: "Code", exact: true }).click();
      await page.getByRole("button", { name: "Rerun this tab", exact: true }).click();
      await expect.poll(() => values(restored.id, 2, "A2:B2")).toEqual([[7, 14]]);
    }

    // A pre-existing tab shifts imported IDs. Literal calc.cell(1, ...) in the command must still read its original input tab.
    const fresh = await (await page.request.post("/api/v1/sheets", { headers: sameOrigin(), data: { title: "Import into existing workbook" } })).json();
    const appended = await page.request.post(`/api/v1/sheets/${fresh.id}/tabs/import?filename=restored.xlsx&mode=new_tab`, { headers: sameOrigin({ "Content-Type": "application/octet-stream" }), data: files.get("xlsx")! });
    expect(appended.ok()).toBeTruthy();
    await page.goto(`/s/${fresh.id}`);
    await expect.poll(() => values(fresh.id, 2, "A1:C1")).toEqual([[5, 10, 11]]);
    await page.request.post(`/api/v1/sheets/${fresh.id}/edits`, { headers: sameOrigin(), data: { edits: [{ type: "set", tab: 2, at: "A1", rows: [["9"]] }] } });
    await expect.poll(() => values(fresh.id, 2, "A1:C1")).toEqual([[9, 18, 19]]);
    await page.getByRole("tab", { name: "Analysis", exact: true }).click();
    await page.getByRole("button", { name: "Code", exact: true }).click();
    await page.getByRole("button", { name: "Rerun this tab", exact: true }).click();
    await expect.poll(() => values(fresh.id, 3, "A2:B2")).toEqual([[9, 18]]);
    expect(errors).toEqual([]);
  } finally { await context.close(); }
});
