import { expect, test } from "@playwright/test";
import { audit } from "./audit";
import { accountsSession, runAccounts, signIn, watchErrors } from "./support";

for (const reducedMotion of ["reduce", "no-preference"] as const) {
  test(`viewer names remain inside the window on hover and keyboard focus (${reducedMotion})`, async ({ browser }) => {
    const context = await accountsSession(browser, "carbon", { reducedMotion, bypassCSP: true });
    const page = await context.newPage();
    const errors = watchErrors(page);
    await signIn(page, runAccounts().carbon.email);
    await page.getByRole("button", { name: "New sheet", exact: true }).click();
    await expect(page.getByTestId("grid")).toBeVisible();
    await expect(page.getByRole("status", { includeHidden: true }).filter({ hasText: /^All changes kept$/ })).toHaveCount(1);
    // Selecting a cell starts this browser's presence heartbeat.
    await page.getByTestId("cell-input").press("ArrowDown");
    const viewers = page.getByRole("group", { name: "Viewing this sheet" });
    const trigger = viewers.locator('[tabindex="0"]').first();
    await expect(trigger).toBeVisible();
    const tooltip = page.getByRole("tooltip");
    // Radix's role=tooltip is the hidden accessible description. Measure its
    // visible bubble wrapper to catch the original off-screen clipping.
    const bubble = page.locator("[data-radix-popper-content-wrapper]").filter({ has: tooltip });
    const fitsBelowAvatar = async () => {
      const [label, avatar] = await Promise.all([bubble.boundingBox(), trigger.boundingBox()]);
      const viewport = page.viewportSize()!;
      return !!(label && avatar && label.width > 20 && label.height > 20 &&
        label.x >= 0 && label.y >= avatar.y + avatar.height &&
        label.x + label.width <= viewport.width && label.y + label.height <= viewport.height);
    };
    // 721px is the narrowest layout that displays the viewer stack.
    for (const width of [1440, 760, 721]) {
      await page.setViewportSize({ width, height: 900 });
      await page.mouse.move(0, 200, { steps: 5 });
      await trigger.hover();
      await expect(tooltip).toContainText(runAccounts().carbon.id);
      await expect.poll(fitsBelowAvatar, `viewer name is fully visible at ${width}px`).toBe(true);
      await expect(bubble.locator('[data-state$="-open"]')).toHaveCSS("opacity", "1");
      await page.screenshot({ path: test.info().outputPath(`viewer-${width}.png`) });
      await page.mouse.move(0, 200, { steps: 5 });
      await expect(tooltip).toHaveCount(0);
    }
    await page.setViewportSize({ width: 1440, height: 900 });
    await trigger.focus();
    await trigger.press("Tab");
    await page.keyboard.press("Shift+Tab");
    await expect(trigger).toBeFocused();
    await expect(tooltip).toContainText(runAccounts().carbon.id);
    await expect.poll(fitsBelowAvatar).toBe(true);
    await audit(page, `keyboard-accessible viewer tooltip (${reducedMotion})`);
    await trigger.press("Escape");
    await expect(tooltip).toHaveCount(0);
    expect(errors).toEqual([]);
    await context.close();
  });
}
