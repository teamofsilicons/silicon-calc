import { expect, test, type Locator } from "@playwright/test";
import { accountsSession, runAccounts, sameOrigin, signIn, watchErrors } from "./support";

async function highlightMatches(menu: Locator, item: Locator, allowHidden = false) {
  const highlight = menu.locator(':scope > span[aria-hidden="true"]').first();
  const [box, target, opacity] = await Promise.all([
    highlight.boundingBox(), item.boundingBox(), highlight.evaluate(node => Number(getComputedStyle(node).opacity)),
  ]);
  return (allowHidden && opacity < 0.01) || !!(box && target && opacity > 0.99 && Math.abs(box.y - target.y) <= 1 && Math.abs(box.height - target.height) <= 1);
}

for (const reducedMotion of ["reduce", "no-preference"] as const) {
  test(`submenu hover and keyboard focus keep each menu's highlight independent (${reducedMotion})`, async ({ browser }) => {
    const context = await accountsSession(browser, "carbon", { reducedMotion });
    const page = await context.newPage();
    const errors = watchErrors(page);
    await signIn(page, runAccounts().carbon.email);
    await page.getByRole("button", { name: "New sheet", exact: true }).click();
    await expect(page.getByTestId("grid")).toBeVisible();
    const sheet = new URL(page.url()).pathname.split("/")[2]!;
    const seed = await page.request.post(`/api/v1/sheets/${sheet}/edits`, {
      headers: sameOrigin(), data: { base_version: 0, edits: [{ type: "add_tab", name: "Hover test", index: 1 }] },
    });
    expect(seed.ok()).toBe(true);
    await page.reload();
    await expect(page.getByRole("status", { includeHidden: true }).filter({ hasText: /^All changes kept$/ })).toHaveCount(1);
    await page.getByRole("tab", { name: "Hover test", exact: true }).click({ button: "right" });
    const colour = page.getByRole("menuitem", { name: "Colour", exact: true });
    const main = page.getByRole("menu").filter({ has: colour });
    await colour.hover();
    const green = page.getByRole("menuitem", { name: "Green", exact: true });
    await expect(green).toBeVisible();
    const submenu = page.getByRole("menu").filter({ has: green });
    for (const name of ["Green", "Red", "Pink", "Green"]) {
      const item = page.getByRole("menuitem", { name, exact: true });
      await item.hover();
      await expect(item).toBeFocused();
      await expect(colour).toHaveAttribute("data-state", "open");
      await expect.poll(() => highlightMatches(submenu, item)).toBe(true);
      await expect.poll(() => highlightMatches(main, colour, true), "a portaled submenu must not move the parent highlight onto another action").toBe(true);
    }
    await page.screenshot({ path: test.info().outputPath("submenu-hover.png") });
    await page.keyboard.press("Escape");
    await expect(main).toBeHidden();
    await page.mouse.move(0, 0);
    await page.getByRole("tab", { name: "Hover test", exact: true }).press("ArrowDown");
    const rename = page.getByRole("menuitem", { name: /^Rename/ });
    await expect(rename).toBeFocused();
    await rename.press("ArrowDown");
    await expect(colour).toBeFocused();
    await colour.press("ArrowRight");
    const noColour = page.getByRole("menuitem", { name: "No colour", exact: true });
    await expect(noColour).toBeFocused();
    await noColour.press("End");
    const pink = page.getByRole("menuitem", { name: "Pink", exact: true });
    await expect(pink).toBeFocused();
    await expect.poll(() => highlightMatches(submenu, pink)).toBe(true);
    await expect.poll(() => highlightMatches(main, colour, true)).toBe(true);
    await pink.press("ArrowLeft");
    await expect(green).toBeHidden();
    await expect(colour).toBeFocused();
    await expect.poll(() => highlightMatches(main, colour)).toBe(true);
    await colour.press("ArrowDown");
    const moveLeft = page.getByRole("menuitem", { name: "Move left", exact: true });
    await expect(moveLeft).toBeFocused();
    await expect.poll(() => highlightMatches(main, moveLeft)).toBe(true);
    const remove = page.getByRole("menuitem", { name: "Delete tab", exact: true });
    await remove.hover();
    await expect.poll(() => highlightMatches(main, remove)).toBe(true);
    await expect(main.locator(':scope > span[aria-hidden="true"]').first()).toHaveAttribute("data-tone", "danger");
    await page.keyboard.press("Escape");
    expect(errors).toEqual([]);
    await context.close();
  });
}
