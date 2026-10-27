import { expect, test, type Browser, type Page } from "@playwright/test";
import { accountsSession, BASE, runAccounts, sameOrigin, signIn } from "./support";

async function openCommands(browser: Browser) {
  const context = await accountsSession(browser, "carbon", { reducedMotion: "reduce" });
  const page = await context.newPage();
  await signIn(page, runAccounts().carbon.email);
  await page.getByRole("button", { name: "New sheet", exact: true }).click();
  await expect(page.getByTestId("grid")).toBeVisible();
  const sheet = new URL(page.url()).pathname.split("/")[2]!;
  await page.getByRole("button", { name: "Code", exact: true }).click();
  return { context, page, sheet };
}

async function enterCommand(page: Page, code: string) {
  await page.getByRole("textbox", { name: "JavaScript command", exact: true }).fill(code);
  await page.getByRole("button", { name: "Run command", exact: true }).click();
}

test("JavaScript previews in the browser and saves literal table results with rerunnable provenance", async ({ browser }) => {
  const { context, page, sheet } = await openCommands(browser);
  await expect(page.getByRole("combobox", { name: "Language", exact: true })).toHaveCount(0);
  const seeded = await page.request.post(`/api/v1/sheets/${sheet}/edits`, { headers: sameOrigin(), data: { base_version: 0, edits: [{ type: "set", tab: 1, at: "A1", rows: [["Region", "Amount"], ["East", "12"], ["West", "24"]] }] } });
  expect(seeded.ok()).toBe(true);
  let commits = 0;
  page.on("request", request => { if (request.url().endsWith(`/sheets/${sheet}/run`)) commits++; });
  await enterCommand(page, 'const tabs = calc.tabs();\nconst cells = await calc.values(tabs[0].id, "A1:B3");\nreturn { columns: ["Count", "Literal"], rows: [[cells.length, "=1+1"]] };');
  await expect(page.getByRole("region", { name: "Command result" })).toContainText("=1+1");
  expect(commits).toBe(0);
  await page.screenshot({ path: "screens/calc-javascript.png" });
  await page.getByRole("combobox", { name: "Put the result" }).click();
  await page.getByRole("option", { name: "In a new tab", exact: true }).click();
  await page.getByRole("textbox", { name: "Tab name", exact: true }).fill("Browser result");
  await page.getByRole("button", { name: "Run command", exact: true }).click();
  await expect(page.getByRole("tab", { name: /Browser result/ })).toHaveAttribute("aria-selected", "true");
  expect(commits).toBe(1);
  const tabsResponse = await page.request.get(`/api/v1/sheets/${sheet}/tabs`);
  const tabs = (await tabsResponse.json()).tabs as Array<{ id: number; name: string; code: { language: string; code: string } | null }>;
  const result = tabs.find(tab => tab.name === "Browser result")!;
  expect(result.code?.language).toBe("javascript");
  expect(result.code?.code).toContain("calc.values");
  const cellsResponse = await page.request.get(`/api/v1/sheets/${sheet}/tabs/${result.id}/cells?range=A1:B2&format=values`);
  expect((await cellsResponse.json()).values).toEqual([["Count", "Literal"], [3, "=1+1"]]);
  await page.getByRole("button", { name: "Rerun this tab", exact: true }).click();
  await expect.poll(() => commits).toBe(2);
  await expect(page.getByRole("button", { name: "Rerun this tab", exact: true })).toBeEnabled();
  await context.close();
});

test("browser commands cannot reach the network or the page credentials", async ({ browser }) => {
  const { context, page } = await openCommands(browser);
  const sandbox = await page.request.get("/code-sandbox");
  expect(sandbox.headers()["content-security-policy"]).toContain("connect-src 'none'");
  expect(sandbox.headers()["content-security-policy"]).not.toContain("unsafe-eval");
  let requests = 0;
  await page.route("**/worker-probe", route => { requests++; return route.fulfill({ status: 200, contentType: "text/javascript", body: "export default 1" }); });
  await enterCommand(page, `
    await new Promise(resolve => setTimeout(resolve, 400));
    let scope = self, nativeFetch;
    while (scope && !nativeFetch) { const descriptor = Object.getOwnPropertyDescriptor(scope, "fetch"); if (typeof descriptor?.value === "function") nativeFetch = descriptor.value; scope = Object.getPrototypeOf(scope); }
    let fetchBlocked = false, importBlocked = false;
    try { await nativeFetch.call(self, ${JSON.stringify(`${BASE}/worker-probe`)}); } catch { fetchBlocked = true; }
    try { await import(${JSON.stringify(`${BASE}/worker-probe`)}); } catch { importBlocked = true; }
    return { columns: ["Native fetch exists", "Fetch blocked", "Import blocked", "Document", "Storage"], rows: [[typeof nativeFetch === "function", fetchBlocked, importBlocked, typeof document, typeof localStorage]] };
  `);
  await expect(page.locator('iframe[title="JavaScript command sandbox"]')).toHaveAttribute("sandbox", "allow-scripts");
  const result = page.getByRole("region", { name: "Command result" });
  await expect(result.locator("tbody tr")).toHaveText("truetruetrueundefinedundefined");
  expect(requests).toBe(0);
  await expect(page.locator('iframe[title="JavaScript command sandbox"]')).toHaveCount(0);
  await context.close();
});

test("infinite commands can be stopped and are terminated at the time limit", async ({ browser }) => {
  const { context, page } = await openCommands(browser);
  await enterCommand(page, "while (true) {}");
  await expect(page.locator('iframe[title="JavaScript command sandbox"]')).toHaveCount(1);
  await page.getByRole("button", { name: "Stop command", exact: true }).click();
  await expect(page.getByRole("complementary", { name: "Code" }).getByRole("alert")).toContainText("The command was stopped");
  await expect(page.locator('iframe[title="JavaScript command sandbox"]')).toHaveCount(0);
  await page.clock.install();
  await enterCommand(page, "while (true) {}");
  await expect(page.locator('iframe[title="JavaScript command sandbox"]')).toHaveCount(1);
  await page.clock.fastForward(60_100);
  await expect(page.getByRole("complementary", { name: "Code" }).getByRole("alert")).toContainText("exceeded 60 seconds");
  await expect(page.locator('iframe[title="JavaScript command sandbox"]')).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Run command", exact: true })).toBeEnabled();
  await enterCommand(page, "return () => 1;");
  await expect(page.getByRole("complementary", { name: "Code" }).getByRole("alert")).toContainText("cloned");
  await context.close();
});
