import { expect, test, type BrowserContext, type Page } from "@playwright/test";
import { audit } from "./audit";
import { accountsSession, runAccounts, sameOrigin, signIn, watchErrors } from "./support";

const csv = (name: string) => ({ name, mimeType: "text/csv", buffer: Buffer.from("item,value\nFirst,12\nSecond,24\n") });
async function throttle(context: BrowserContext, page: Page, rate: number) {
  const network = await context.newCDPSession(page);
  await network.send("Network.enable");
  await network.send("Network.emulateNetworkConditions", { offline: false, latency: 0, downloadThroughput: -1, uploadThroughput: rate });
  return network;
}
async function cleanup(context: BrowserContext, names: string[]) {
  await context.setOffline(false);
  const jobs = await imports(context).catch(() => []);
  for (const job of jobs) if (names.includes(job.request.filename)) await context.request.delete(`/api/v1/imports/${job.id}`, { headers: sameOrigin() }).catch(() => undefined);
}
async function imports(context: BrowserContext) { return (await (await context.request.get("/api/v1/imports")).json()).imports as { id: string; request: { filename: string }; state: string; updated_at: number; uploaded: number; chunks: number[]; result?: { sheet: string; tabs: number[] } }[]; }

test("only active and recently finished imports appear, including after reload", async ({ browser }) => {
  const context = await accountsSession(browser, "friend", { reducedMotion: "reduce" });
  const page = await context.newPage();
  const names = ["recent-result.csv", "recent-failure.xlsx", "still-uploading.csv"];
  try {
    await signIn(page, runAccounts().friend.email);
    const panel = page.getByRole("region", { name: "Import progress" });
    await expect(panel).toHaveCount(0);
    await page.locator('input[type="file"]').setInputFiles(csv(names[0]));
    await page.locator('input[type="file"]').setInputFiles({ name: names[1], mimeType: "application/octet-stream", buffer: Buffer.from("Invalid workbook") });
    await expect(panel.locator(`[aria-label="${names[0]}"]`).getByRole("status")).toHaveText("Imported");
    await expect(panel.locator(`[aria-label="${names[1]}"]`).getByRole("button", { name: "Retry", exact: true })).toBeVisible();
    const finished = (await imports(context)).filter(job => names.includes(job.request.filename));
    const expires = Math.max(...finished.map(job => job.updated_at)) + 120_000;
    await panel.getByRole("button", { name: "Imports · Minimize", exact: true }).click();
    await page.clock.setFixedTime(new Date(expires - 10_000));
    await expect(panel).toBeVisible();
    await page.clock.setFixedTime(new Date(expires + 1));
    await expect(panel).toHaveCount(0);
    await page.reload();
    await expect(page.getByRole("heading", { name: "Sheets", exact: true })).toBeVisible();
    await expect(panel).toHaveCount(0);
    await throttle(context, page, 64 * 1024);
    await page.locator('input[type="file"]').setInputFiles({ ...csv(names[2]), buffer: Buffer.from("x\n" + "1\n".repeat(2000000)) });
    await expect(panel).toContainText(names[2]);
    await expect(panel).not.toContainText(names[0]);
    await expect(panel).not.toContainText(names[1]);
    await page.clock.setFixedTime(new Date(expires + 86400_000));
    await expect(panel.getByRole("progressbar", { name: "File upload" })).toBeVisible();
    await panel.getByRole("button", { name: "Cancel import", exact: true }).click();
    await expect(panel).toHaveCount(0);
  } finally { await cleanup(context, names); await context.close(); }
});

test("imports survive navigation, refresh, and closing the uploading tab without stealing navigation", async ({ browser }) => {
  const context = await accountsSession(browser, "carbon", { reducedMotion: "reduce" });
  let page = await context.newPage();
  const errors = watchErrors(page);
  await signIn(page, runAccounts().carbon.email);
  const existing = await (await page.request.post("/api/v1/sheets", { headers: sameOrigin(), data: { title: "Work during import" } })).json();
  await page.reload();
  let network = await throttle(context, page, 256 * 1024);
  const name = "persistent-progress.csv";
  try {
    await page.locator('input[type="file"]').setInputFiles({ ...csv(name), buffer: Buffer.from("item,value\n" + "First,12\nSecond,24\n".repeat(200000)) });
    let panel = page.getByRole("region", { name: "Import progress" });
    await expect(panel).toContainText(name);
    const upload = panel.locator(`[aria-label="${name}"]`).getByRole("progressbar", { name: "File upload" });
    await expect.poll(async () => Number(await upload.getAttribute("aria-valuenow"))).toBeGreaterThan(0);
    await expect(page.getByRole("button", { name: "Import", exact: true })).toBeEnabled();
    await expect(page.getByRole("button", { name: "New sheet", exact: true })).toBeEnabled();
    await page.getByRole("link", { name: "Work during import", exact: true }).click();
    await expect(page.getByTestId("grid")).toBeVisible();
    await expect(panel).toContainText(name);
    await expect.poll(async () => (await imports(context)).find(job => job.request.filename === name)?.uploaded ?? 0).toBeGreaterThanOrEqual(1024 * 1024);
    await page.reload();
    await expect(page.getByTestId("grid")).toBeVisible();
    await expect(panel).toContainText(name);
    await expect.poll(async () => Number(await panel.locator(`[aria-label="${name}"]`).getByRole("progressbar", { name: "File upload" }).getAttribute("aria-valuenow"))).toBeGreaterThan(20);
    await page.close();
    page = await context.newPage();
    network = await throttle(context, page, 256 * 1024);
    await page.goto(`/s/${existing.id}`);
    panel = page.getByRole("region", { name: "Import progress" });
    await expect(panel).toContainText(name);
    await expect(panel).toContainText("Saved on this device");
    await audit(page, "Persistent file import");
    await page.screenshot({ path: test.info().outputPath("import-while-editing.png") });
    await network.send("Network.emulateNetworkConditions", { offline: false, latency: 0, downloadThroughput: -1, uploadThroughput: -1 });
    await expect(panel.locator(`[aria-label="${name}"]`).getByRole("link", { name: "Open sheet" })).toBeVisible({ timeout: 45_000 });
    await expect(page).toHaveURL(new RegExp(`/s/${existing.id}$`));
    const done = (await imports(context)).find(job => job.request.filename === name)!;
    const cells = await (await page.request.get(`/api/v1/sheets/${done.result!.sheet}/tabs/${done.result!.tabs[0]}/cells?range=A1:B3&format=values`)).json();
    expect(cells.values).toEqual([["item", "value"], ["First", 12], ["Second", 24]]);
    // Reopening the page must discover the same receipt rather than create another sheet.
    await page.reload();
    await expect(panel.locator(`[aria-label="${name}"]`).getByRole("link", { name: "Open sheet" })).toBeVisible();
    expect((await imports(context)).filter(job => job.request.filename === name)).toHaveLength(1);
    expect(errors).toEqual([]);
    await panel.getByRole("button", { name: `Dismiss ${name}` }).click();
  } finally { await cleanup(context, [name]); await context.close(); }
});

test("a lost connection resumes automatically and server processing survives closing Calc", async ({ browser }) => {
  const context = await accountsSession(browser, "carbon", { reducedMotion: "reduce" });
  let page = await context.newPage();
  await signIn(page, runAccounts().carbon.email);
  const network = await throttle(context, page, 128 * 1024);
  const name = "offline-recovery.csv";
  try {
    await page.locator('input[type="file"]').setInputFiles({ ...csv(name), buffer: Buffer.from("item,value\n" + "First,12\nSecond,24\n".repeat(90000)) });
    let panel = page.getByRole("region", { name: "Import progress" });
    await expect.poll(async () => Number(await panel.locator(`[aria-label="${name}"]`).getByRole("progressbar", { name: "File upload" }).getAttribute("aria-valuenow"))).toBeGreaterThan(0);
    await context.setOffline(true);
    await expect(panel).toContainText(/Offline|connection|Connection/);
    await network.send("Network.emulateNetworkConditions", { offline: false, latency: 0, downloadThroughput: -1, uploadThroughput: -1 });
    const started = page.waitForResponse(response => /\/imports\/[^/]+\/start$/.test(response.url()) && response.ok());
    await context.setOffline(false);
    await started;
    const job = (await imports(context)).find(job => job.request.filename === name)!;
    await page.close();
    await expect.poll(async () => (await (await context.request.get(`/api/v1/imports/${job.id}`)).json()).state).toBe("completed");
    page = await context.newPage();
    await page.goto("/sheets");
    panel = page.getByRole("region", { name: "Import progress" });
    const item = panel.locator(`[aria-label="${name}"]`);
    await expect(item.getByRole("status")).toHaveText("Imported");
    await item.getByRole("link", { name: "Open sheet" }).click();
    await expect(page.getByTestId("grid")).toBeVisible();
    await panel.getByRole("button", { name: /Imports.*Expand/ }).click();
    await item.getByRole("button", { name: `Dismiss ${name}` }).click();
  } finally { await cleanup(context, [name]); await context.close(); }
});

test("one failed file does not block other imports, editing, or additional queued files", async ({ browser }) => {
  const context = await accountsSession(browser, "carbon", { reducedMotion: "reduce" });
  const page = await context.newPage();
  await signIn(page, runAccounts().carbon.email);
  await page.getByRole("button", { name: "New sheet", exact: true }).click();
  await expect(page.getByTestId("grid")).toBeVisible();
  try {
    await page.locator('input[type="file"]').first().setInputFiles([csv("kept.csv"), { name: "broken.xlsx", mimeType: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", buffer: Buffer.from("This is not a workbook") }, csv("after-error.csv")]);
    const panel = page.getByRole("region", { name: "Import progress" });
    await expect(panel).toContainText("kept.csv");
    await page.getByRole("button", { name: "Sheet actions", exact: true }).click();
    await expect(page.getByRole("menuitem", { name: "Import as new tabs", exact: true })).toBeEnabled();
    await page.keyboard.press("Escape");
    await expect(page.getByRole("tab", { name: "kept", exact: true })).toBeVisible();
    await expect(page.getByRole("tab", { name: "after-error", exact: true })).toBeVisible();
    const failed = panel.locator('[aria-label="broken.xlsx"]');
    await expect(failed.getByRole("button", { name: "Retry", exact: true })).toBeVisible();
    await page.reload();
    await expect(failed.getByRole("button", { name: "Retry", exact: true })).toBeVisible();
    await page.locator('input[type="file"]').first().setInputFiles(csv("another.csv"));
    await expect(page.getByRole("tab", { name: "another", exact: true })).toBeVisible();
    for (const job of await imports(context)) if (["kept.csv", "broken.xlsx", "after-error.csv", "another.csv"].includes(job.request.filename)) await page.request.delete(`/api/v1/imports/${job.id}`, { headers: sameOrigin() });
  } finally { await cleanup(context, ["kept.csv", "broken.xlsx", "after-error.csv", "another.csv"]); await context.close(); }
});

test("another open tab takes over a saved upload and cancellation removes the queued source", async ({ browser }) => {
  const context = await accountsSession(browser, "carbon", { reducedMotion: "reduce" });
  const uploader = await context.newPage();
  await signIn(uploader, runAccounts().carbon.email);
  await throttle(context, uploader, 128 * 1024);
  const name = "two-tab-recovery.csv";
  let observer: Page | null = null;
  try {
    await uploader.locator('input[type="file"]').setInputFiles({ ...csv(name), buffer: Buffer.from("item,value\n" + "First,12\nSecond,24\n".repeat(160000)) });
    await expect(uploader.getByRole("region", { name: "Import progress" })).toContainText(name);
    observer = await context.newPage();
    const network = await throttle(context, observer, 256 * 1024);
    await observer.goto("/sheets");
    const panel = observer.getByRole("region", { name: "Import progress" });
    await expect(panel).toContainText(name);
    await expect.poll(async () => (await imports(context)).find(job => job.request.filename === name)?.uploaded ?? 0).toBeGreaterThanOrEqual(1024 * 1024);
    await uploader.close();
    await network.send("Network.emulateNetworkConditions", { offline: false, latency: 0, downloadThroughput: -1, uploadThroughput: -1 });
    await expect(panel.locator(`[aria-label="${name}"]`).getByRole("status")).toHaveText("Imported", { timeout: 30_000 });
    const jobs = (await imports(context)).filter(job => job.request.filename === name);
    expect(jobs).toHaveLength(1);
    await panel.getByRole("button", { name: `Dismiss ${name}` }).click();
    await network.send("Network.emulateNetworkConditions", { offline: false, latency: 0, downloadThroughput: -1, uploadThroughput: 64 * 1024 });
    const cancelName = "cancel-upload.csv";
    await observer.locator('input[type="file"]').setInputFiles({ ...csv(cancelName), buffer: Buffer.from("x\n" + "1\n".repeat(1000000)) });
    const cancelled = panel.locator(`[aria-label="${cancelName}"]`);
    await expect(cancelled.getByRole("button", { name: "Cancel import" })).toBeVisible();
    await cancelled.getByRole("button", { name: "Cancel import" }).click();
    await expect(cancelled).toHaveCount(0);
    await observer.reload();
    await expect(panel).toHaveCount(0);
  } finally { await cleanup(context, [name, "cancel-upload.csv"]); await context.close(); }
});
