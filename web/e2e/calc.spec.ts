import { expect, test } from "@playwright/test";
import { audit } from "./audit";
import { accountsSession, runAccounts, sameOrigin, signIn, watchErrors, BASE } from "./support";

test("a sheet keeps keyboard edits, formulas, comments, history, copies and public reads", async ({ browser }) => {
  const context = await accountsSession(browser, "carbon");
  const page = await context.newPage();
  const errors = watchErrors(page);
  await signIn(page, runAccounts().carbon.email);
  await page.getByRole("button", { name: "New sheet", exact: true }).click();
  await expect(page.getByTestId("grid")).toBeVisible();
  const sheetPath = new URL(page.url()).pathname;
  const sheetId = sheetPath.split("/")[2];
  await expect(page).toHaveTitle("Untitled sheet · Calc");
  const fileName = "Quarterly <Sales> & growth";
  await page.getByRole("button", { name: "Sheet title: Untitled sheet", exact: true }).click();
  const titleInput = page.getByRole("textbox", { name: "Sheet title", exact: true });
  await titleInput.fill(fileName);
  await titleInput.press("Enter");
  await expect(page).toHaveTitle(`${fileName} · Calc`);
  // Check server metadata too, independently of the client-side rename effect.
  const rendered = await page.request.get(sheetPath);
  expect(await rendered.text()).toContain("<title>Quarterly &lt;Sales&gt; &amp; growth · Calc</title>");
  await page.reload();
  await expect(page).toHaveTitle(`${fileName} · Calc`);
  await expect(page.getByRole("status").filter({ hasText: /^All changes kept$/ })).toBeVisible();
  await page.getByTestId("cell-input").focus();
  await page.keyboard.type("12"); await page.keyboard.press("Enter");
  await page.keyboard.type("=A1*2"); await page.keyboard.press("Enter");
  await expect.poll(async () => { const response = await page.request.get(`/api/v1/sheets/${sheetId}/tabs/1/cells?range=A1:A2&format=values`); return (await response.json()).values; }).toEqual([[12], [24]]);
  await page.getByTestId("name-box").fill("A2"); await page.getByTestId("name-box").press("Enter");
  await expect(page.getByTestId("formula-input")).toHaveValue("=A1*2");
  await page.getByRole("button", { name: "Comments", exact: true }).click();
  await page.getByRole("button", { name: "New comment", exact: true }).click();
  await page.getByRole("combobox", { name: "Comment", exact: true }).fill("Please check this total.");
  await page.getByRole("button", { name: "Comment", exact: true }).click();
  await expect(page.getByTestId("comment-thread")).toContainText("Please check this total.");
  await page.getByRole("button", { name: "History", exact: true }).click();
  await expect(page.getByTestId("version").first()).toBeVisible();
  await page.getByRole("button", { name: "Copies", exact: true }).click();
  await page.getByRole("textbox", { name: "Name of the copy" }).fill("Try a different total");
  await page.getByRole("button", { name: "Make a copy", exact: true }).click();
  await expect(page).toHaveURL(/\/c\/[a-z0-9]+$/);
  await expect(page).toHaveTitle(`${fileName} · Calc`);
  await page.getByRole("link", { name: "Open parent", exact: true }).click();
  await expect(page).toHaveURL(`${BASE}${sheetPath}`);
  const anonymous = await browser.newContext();
  const visitor = await anonymous.newPage();
  await visitor.goto(`${BASE}${sheetPath}`);
  await expect(visitor).toHaveTitle("Sheet · Calc");
  expect(await visitor.content()).not.toContain("Quarterly");
  const publicAnswer = await page.request.put(`/api/v1/sheets/${sheetId}/public`, { headers: sameOrigin(), data: { enabled: true, access: "view" } });
  expect(publicAnswer.ok()).toBe(true);
  await visitor.reload();
  await expect(visitor).toHaveTitle(`${fileName} · Calc`);
  await expect(visitor.getByTestId("grid")).toBeVisible();
  await expect(visitor.getByText("Viewing this sheet", { exact: true })).toBeVisible();
  await expect(visitor.getByRole("link", { name: "Sign in", exact: true })).toBeVisible();
  await visitor.getByTestId("name-box").fill("A2"); await visitor.getByTestId("name-box").press("Enter");
  await expect(visitor.getByTestId("formula-input")).toHaveValue("=A1*2");
  await anonymous.close();
  await page.goto("/sheets");
  await expect(page).toHaveTitle("Sheets · Calc");
  expect(errors).toEqual([]);
  await context.close();
});

test("folders and file imports are reachable from the workspace", async ({ browser }) => {
  const context = await accountsSession(browser, "carbon");
  const page = await context.newPage();
  await signIn(page, runAccounts().carbon.email);
  await page.getByRole("button", { name: "New folder", exact: true }).click();
  const folderName = `Data ${Date.now()}`;
  await page.getByRole("textbox", { name: "Name", exact: true }).fill(folderName);
  await page.getByRole("dialog").getByRole("button", { name: "New folder", exact: true }).click();
  await page.getByRole("link", { name: folderName, exact: true }).click();
  await expect(page.getByRole("heading", { level: 1 })).toHaveText(folderName);
  await page.locator('input[type="file"]').setInputFiles({ name: "revenue.csv", mimeType: "text/csv", buffer: Buffer.from("region,amount\nEast,12\nWest,24\n") });
  await page.getByRole("region", { name: "Import progress" }).getByLabel("revenue.csv", { exact: true }).getByRole("link", { name: "Open sheet" }).click();
  await expect(page.getByTestId("grid")).toBeVisible();
  await page.getByRole("button", { name: "Code", exact: true }).click();
  await page.getByRole("textbox", { name: "JavaScript command", exact: true }).fill('return await calc.tab("revenue");');
  await page.getByRole("button", { name: "Run command", exact: true }).click();
  await expect(page.getByRole("region", { name: "Command result" })).toContainText("West");
  await context.close();
});

test("charts preserve numeric scatter spacing, grouped measures and negative bars", async ({ browser }) => {
  const context = await accountsSession(browser, "carbon", { bypassCSP: true, reducedMotion: "reduce" });
  const page = await context.newPage();
  const errors = watchErrors(page);
  await signIn(page, runAccounts().carbon.email);
  await page.getByRole("button", { name: "New sheet", exact: true }).click();
  await expect(page.getByTestId("grid")).toBeVisible();
  const sheetId = new URL(page.url()).pathname.split("/")[2];
  const response = await page.request.post(`/api/v1/sheets/${sheetId}/edits`, { headers: sameOrigin(), data: {
    base_version: 0,
    edits: [
      { type: "set", tab: 1, at: "A1", rows: [["Distance", "Time", "Cost"], ["1", "20", "-3"], ["2", "21", "0"], ["100", "40", "8"]] },
      ...(["scatter", "bar", "column"] as const).map((kind, index) => ({ type: "chart", chart: { tab: 1, kind, title: `${kind} measures`, range: "A1:C4", anchor: ["A6", "E6", "I6"][index], width: 360, height: 260 } })),
    ],
  } });
  expect(response.ok()).toBe(true);
  const scatter = page.getByRole("figure", { name: "Chart: scatter measures", exact: true });
  await expect(scatter.locator("circle[data-series]")).toHaveCount(6);
  const positions = await scatter.locator('circle[data-series="Time"]').evaluateAll(points => points.map(point => Number(point.getAttribute("cx"))));
  expect((positions[1]! - positions[0]!) / (positions[2]! - positions[0]!)).toBeCloseTo(1 / 99, 5);
  const bars = page.getByRole("figure", { name: "Chart: bar measures", exact: true });
  await expect(bars.locator('rect[data-series="Cost"]')).toHaveCount(3);
  expect(Number(await bars.locator('rect[data-value="-3"]').getAttribute("width"))).toBeGreaterThan(0);
  const columns = page.getByRole("figure", { name: "Chart: column measures", exact: true });
  await expect(columns.locator('rect[data-series="Time"]')).toHaveCount(3);
  expect(Number(await columns.locator('rect[data-value="-3"]').getAttribute("height"))).toBeGreaterThan(0);
  await audit(page, "numeric scatter and grouped signed bars");
  await page.screenshot({ path: "screens/calc-charts.png" });
  expect(errors).toEqual([]);
  await context.close();
});
