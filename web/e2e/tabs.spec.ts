import { expect, test, type Browser, type Page } from "@playwright/test";
import { audit } from "./audit";
import { accountsSession, runAccounts, sameOrigin, signIn, watchErrors } from "./support";

async function sheetWithTabs(browser: Browser, mobile = false, extraTabs = 0) {
  const context = await accountsSession(browser, "carbon", {
    bypassCSP: true, reducedMotion: "reduce",
    ...(mobile ? { viewport: { width: 390, height: 844 }, hasTouch: true, isMobile: true } : {}),
  });
  const page = await context.newPage();
  const errors = watchErrors(page);
  await signIn(page, runAccounts().carbon.email);
  await page.getByRole("button", { name: "New sheet", exact: true }).click();
  await expect(page.getByTestId("grid")).toBeVisible();
  const sheet = new URL(page.url()).pathname.split("/")[2]!;
  const seed = await page.request.post(`/api/v1/sheets/${sheet}/edits`, {
    headers: sameOrigin(), data: { base_version: 0, edits: [
      { type: "rename_tab", tab: 1, name: "Overview" },
      { type: "set", tab: 1, at: "A1", rows: [["Overview value"]] },
      { type: "add_tab", name: "Revenue", index: 1 },
      { type: "add_tab", name: "Costs", index: 2 },
      ...Array.from({ length: extraTabs }, (_, index) => ({ type: "add_tab", name: `Month ${index + 1}`, index: index + 3 })),
    ] },
  });
  expect(seed.ok()).toBe(true);
  await page.reload();
  await expect(page.getByRole("tab", { name: "Overview", exact: true })).toHaveAttribute("aria-selected", "true");
  // Tab attributes and the initial cell are server-rendered. Wait for the live
  // session effect before typing so hydration cannot move focus mid-input.
  await expect(page.getByRole("status", { includeHidden: true }).filter({ hasText: /^All changes kept$/ })).toHaveCount(1);
  await expect(page.getByTestId("formula-input")).toHaveValue("Overview value");
  return { context, page, errors, sheet };
}

const tab = (page: Page, name: string) => page.getByRole("tab", { name, exact: true });
const menu = (page: Page, name: string) => page.getByRole("menu", { name, exact: true });

test("the whole tab selects and opens its menu while keyboard, rename and drag still work", async ({ browser }) => {
  const { context, page, errors } = await sheetWithTabs(browser);
  await tab(page, "Revenue").click();
  await expect(tab(page, "Revenue")).toHaveAttribute("aria-selected", "true");
  await expect(menu(page, "Revenue")).toBeVisible();
  await expect(page.getByRole("menuitem", { name: /^Rename/ })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(menu(page, "Revenue")).toHaveCount(0);
  // The arrow is part of the same target, and an already-selected tab opens too.
  await tab(page, "Revenue").locator("[data-tab-chevron]").click();
  await expect(menu(page, "Revenue")).toBeVisible();
  await expect(tab(page, "Revenue")).toHaveAttribute("aria-selected", "true");
  // One click switches both the sheet and its actions while a different menu is open.
  await tab(page, "Costs").click();
  await expect(tab(page, "Costs")).toHaveAttribute("aria-selected", "true");
  await expect(menu(page, "Revenue")).toHaveCount(0);
  await expect(menu(page, "Costs")).toBeVisible();
  await expect(page.getByRole("menu")).toHaveCount(1);
  await page.keyboard.press("Escape");
  await expect(tab(page, "Costs")).toBeFocused();

  await tab(page, "Costs").press("Home");
  await expect(tab(page, "Overview")).toBeFocused();
  await expect(tab(page, "Overview")).toHaveAttribute("aria-selected", "true");
  await tab(page, "Overview").press("ArrowRight");
  await expect(tab(page, "Revenue")).toBeFocused();
  await tab(page, "Revenue").press("Enter");
  await expect(menu(page, "Revenue")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(tab(page, "Revenue")).toBeFocused();
  await tab(page, "Revenue").press("End");
  await expect(tab(page, "Costs")).toBeFocused();
  await tab(page, "Costs").press("ArrowDown");
  await expect(page.getByRole("menuitem", { name: /^Rename/ })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(tab(page, "Costs")).toBeFocused();

  await tab(page, "Costs").press("F2");
  const rename = page.getByRole("textbox", { name: "New name for the tab Costs", exact: true });
  await rename.fill("Do not keep this");
  await rename.press("Escape");
  await expect(tab(page, "Costs")).toBeVisible();
  await expect(tab(page, "Do not keep this")).toHaveCount(0);
  await tab(page, "Revenue").dblclick();
  const revenueName = page.getByRole("textbox", { name: "New name for the tab Revenue", exact: true });
  await revenueName.fill("Sales");
  await revenueName.press("Enter");
  await expect(tab(page, "Sales")).toBeVisible();
  await page.reload();
  await expect(tab(page, "Sales")).toBeVisible();
  await tab(page, "Sales").click({ button: "right" });
  await page.getByRole("menuitem", { name: "Move left", exact: true }).click();
  await expect(page.getByRole("tab").first()).toHaveText("Sales");
  const dropTarget = await tab(page, "Costs").boundingBox();
  expect(dropTarget).not.toBeNull();
  await tab(page, "Sales").dragTo(tab(page, "Costs"), { targetPosition: { x: dropTarget!.width - 2, y: dropTarget!.height / 2 } });
  await expect(page.getByRole("tab").last()).toHaveText("Sales");
  await tab(page, "Sales").hover();
  await expect(page.getByRole("tooltip")).toHaveCount(0);
  await audit(page, "unified tab controls and keyboard navigation");
  await page.screenshot({ path: "screens/calc-tabs-desktop.png" });
  expect(errors).toEqual([]);
  await context.close();
});

test("switching tabs keeps a pending edit and restores each selection and viewport", async ({ browser }) => {
  const { context, page, errors, sheet } = await sheetWithTabs(browser);
  const address = page.getByTestId("name-box");
  const scroller = page.getByTestId("grid-scroller");
  const cellInput = page.getByTestId("cell-input");
  // Enter returns focus to the grid synchronously. Exercise that transition in
  // one frame so the address field's queued selection cannot steal it back.
  await page.evaluate(async () => {
    document.querySelector<HTMLInputElement>('[data-testid="name-box"]')!.focus();
    document.querySelector<HTMLTextAreaElement>('[data-testid="cell-input"]')!.focus();
    await new Promise<void>(resolve => requestAnimationFrame(() => resolve()));
  });
  await expect(cellInput).toBeFocused();
  await address.fill("Z120"); await address.press("Enter");
  await expect.poll(() => scroller.evaluate(element => element.scrollTop)).toBeGreaterThan(1000);
  await expect.poll(() => scroller.evaluate(element => element.scrollLeft)).toBeGreaterThan(0);
  const overviewScroll = await scroller.evaluate(element => ({ top: element.scrollTop, left: element.scrollLeft }));
  await cellInput.focus();
  await page.keyboard.type("42");
  await expect(cellInput).toHaveValue("42");
  await expect(address).toHaveValue("Z120");
  await tab(page, "Revenue").click();
  await expect(address).toHaveValue("A1");
  await address.fill("B8"); await address.press("Enter");
  await tab(page, "Overview").click();
  await expect(address).toHaveValue("Z120");
  await expect(page.getByTestId("formula-input")).toHaveValue("42");
  await expect.poll(() => scroller.evaluate(element => ({ top: element.scrollTop, left: element.scrollLeft }))).toEqual(overviewScroll);
  await tab(page, "Revenue").click();
  await expect(address).toHaveValue("B8");
  await tab(page, "Overview").click();
  await expect(address).toHaveValue("Z120");
  await expect.poll(async () => {
    const response = await page.request.get(`/api/v1/sheets/${sheet}/tabs/1/cells?range=Z120&format=values`);
    return (await response.json()).values;
  }).toEqual([[42]]);
  await tab(page, "Revenue").click();
  await menu(page, "Revenue").getByRole("menuitem", { name: "Delete tab", exact: true }).click();
  await page.getByRole("button", { name: "Hold to delete tab", exact: true }).focus();
  await page.keyboard.down("Space");
  try {
    await expect(page.getByRole("dialog", { name: "Delete tab", exact: true })).toBeHidden();
  } finally {
    await page.keyboard.up("Space");
  }
  await expect(tab(page, "Revenue")).toHaveCount(0);
  await expect(tab(page, "Overview")).toHaveAttribute("aria-selected", "true");
  await expect(address).toHaveValue("Z120");
  await expect(page.getByTestId("formula-input")).toHaveValue("42");
  await expect.poll(() => scroller.evaluate(element => ({ top: element.scrollTop, left: element.scrollLeft }))).toEqual(overviewScroll);
  expect(errors).toEqual([]);
  await context.close();
});

test("touch tab controls stay usable with many tabs and in a read-only sheet", async ({ browser }) => {
  const { context, page, errors, sheet } = await sheetWithTabs(browser, true, 8);
  const publicAnswer = await page.request.put(`/api/v1/sheets/${sheet}/public`, {
    headers: sameOrigin(), data: { enabled: true, access: "view" },
  });
  expect(publicAnswer.ok()).toBe(true);
  for (const width of [390, 320]) {
    await page.setViewportSize({ width, height: 844 });
    await tab(page, "Overview").tap();
    await tab(page, "Revenue").tap();
    await expect(tab(page, "Revenue")).toHaveAttribute("aria-selected", "true");
    const box = await tab(page, "Revenue").boundingBox();
    expect(box?.width).toBeGreaterThanOrEqual(44);
    expect(box?.height).toBeGreaterThanOrEqual(44);
    await expect(menu(page, "Revenue")).toBeVisible();
    await expect(menu(page, "Revenue").getByRole("menuitem", { name: /^Rename/ })).toBeVisible();
    await page.keyboard.press("Escape");
    await tab(page, "Revenue").locator("[data-tab-chevron]").tap();
    await expect(menu(page, "Revenue")).toBeVisible();
    await tab(page, "Costs").tap();
    await expect(tab(page, "Costs")).toHaveAttribute("aria-selected", "true");
    await expect(menu(page, "Revenue")).toHaveCount(0);
    await expect(menu(page, "Costs")).toBeVisible();
    await expect(page.getByRole("menu")).toHaveCount(1);
    await page.keyboard.press("Escape");
    await expect(tab(page, "Costs")).toBeFocused();
    await tab(page, "Costs").press("End");
    await expect(tab(page, "Month 8")).toBeFocused();
    await expect(tab(page, "Month 8")).toBeInViewport();
    if (width === 390) {
      await tab(page, "Month 8").press("F2");
      const rename = page.getByRole("textbox", { name: "New name for the tab Month 8", exact: true });
      await expect(rename).toBeFocused();
      await page.getByRole("tablist", { name: "Tabs", exact: true }).evaluate(element => { element.scrollLeft -= 40; });
      const aligned = async () => {
        const target = await tab(page, "Month 8").boundingBox();
        const input = await rename.boundingBox();
        if (!target || !input) return false;
        const expected = Math.max(8, Math.min(target.x, page.viewportSize()!.width - input.width - 8));
        return Math.abs(input.x - expected) < 1 && Math.abs(input.y - target.y) < 1;
      };
      await expect.poll(aligned).toBe(true);
      await page.setViewportSize({ width: 320, height: 844 });
      await expect.poll(aligned).toBe(true);
      await rename.press("Escape");
      await page.setViewportSize({ width, height: 844 });
    }
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    await audit(page, `many tabs at ${width}px`);
  }
  await page.screenshot({ path: "screens/calc-tabs-phone.png" });
  const visitor = await browser.newContext({ baseURL: new URL(page.url()).origin, bypassCSP: true });
  const readOnly = await visitor.newPage();
  await readOnly.goto(`/s/${sheet}`);
  await tab(readOnly, "Revenue").click();
  await expect(tab(readOnly, "Revenue")).toHaveAttribute("aria-selected", "true");
  await expect(menu(readOnly, "Revenue")).toBeVisible();
  await expect(readOnly.getByRole("menuitem", { name: /^Rename/ })).toHaveAttribute("aria-disabled", "true");
  await expect(readOnly.getByRole("menuitem", { name: "Delete tab", exact: true })).toHaveAttribute("aria-disabled", "true");
  await visitor.close();
  expect(errors).toEqual([]);
  await context.close();
});
