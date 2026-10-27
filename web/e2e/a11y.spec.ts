/**
 * Accessibility: axe-core's WCAG 2.2 A and AA rules on every page, in the light theme, desktop and phone, plus the keyboard
 * path through the shell. A violation fails with the rule, the element and how to fix it. (The CSP is bypassed only so
 * axe's script can be injected.)
 */
import { expect, test, type Browser, type BrowserContext } from "@playwright/test";
import { accountsSession, appConfig, BASE, runAccounts, sameOrigin, signIn } from "./support";
import { audit } from "./audit";

test.describe.configure({ mode: "serial" });

let signedIn: BrowserContext;

async function contextFor(browser: Browser, theme: "light" | "dark", mobile: boolean, storageState?: Awaited<ReturnType<BrowserContext["storageState"]>>) {
  return browser.newContext({ baseURL: BASE, bypassCSP: true, colorScheme: theme, reducedMotion: "reduce", viewport: mobile ? { width: 390, height: 844 } : { width: 1440, height: 900 }, storageState });
}

test.beforeAll(async ({ browser }) => {
  signedIn = await accountsSession(browser, "carbon");
  const page = await signedIn.newPage();
  await signIn(page, runAccounts().carbon.email, appConfig.home);
  await page.close();
});

test.afterAll(async () => {
  await signedIn?.close();
});

for (const theme of ["light"] as const) {
  for (const mobile of [false, true]) {
    const where = `${theme}, ${mobile ? "phone" : "desktop"}`;
    test(`public pages pass WCAG 2.2 AA (${where})`, async ({ browser }) => {
      const context = await contextFor(browser, theme, mobile);
      const page = await context.newPage();
      for (const path of ["/", "/sign-in", "/sign-in?error=access_denied", "/sign-in?reason=session_ended", "/no/such/page"]) {
        await page.goto(path);
        await audit(page, `${path} (${where})`);
      }
      await context.close();
    });

    test(`workspace pages pass WCAG 2.2 AA (${where})`, async ({ browser }) => {
      const context = await contextFor(browser, theme, mobile, await signedIn.storageState());
      const page = await context.newPage();
      for (const item of appConfig.nav) {
        await page.goto(item.href);
        await audit(page, `${item.href} (${where})`);
      }
      await page.goto("/sheets");
      await page.getByRole("button", { name: "New sheet", exact: true }).click();
      await page.getByTestId("grid").waitFor();
      const sheetId = new URL(page.url()).pathname.split("/")[2];
      const seeded = await page.request.post(`/api/v1/sheets/${sheetId}/edits`, { headers: sameOrigin(), data: { base_version: 0, edits: [{ type: "set", tab: 1, at: "A1", rows: [["Region", "Revenue"], ["East", "1200"], ["West", "2400"], ["Total", "=SUM(B2:B3)"]] }] } });
      expect(seeded.ok()).toBe(true);
      await expect(page.getByTestId("formula-input")).toHaveValue("Region");
      await audit(page, `sheet (${where})`);
      await page.screenshot({ path: `screens/calc-${mobile ? "phone" : "desktop"}.png` });
      if (!mobile) await page.screenshot({ path: "screens/calc-workspace.png" });
      await page.getByRole("button", { name: "Share", exact: true }).click();
      await page.getByRole("dialog").waitFor();
      await audit(page, `sheet sharing (${where})`);
      await context.close();
    });
  }
}

test("the keyboard reaches everything in the shell, and focus is always visible", async ({ browser }) => {
  const context = await contextFor(browser, "light", false, await signedIn.storageState());
  const page = await context.newPage();
  await page.goto(appConfig.home);
  await page.keyboard.press("Tab");
  await expect(page.getByRole("link", { name: "Skip to content" })).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(page.locator("main#main")).toBeFocused();
  // Every focus stop shows where it is: a fill, an edge or an underline that differs from its resting look (compared
  // with transitions off, so a fading fill is not read mid-way).
  await page.goto(appConfig.home);
  await page.addStyleTag({ content: "*, *::before, *::after { transition: none !important; animation: none !important; }" });
  const unseen: string[] = [];
  for (let step = 0; step < 14; step++) {
    await page.keyboard.press("Tab");
    const visible = await page.evaluate(() => {
      const element = document.activeElement as HTMLElement | null;
      if (!element || element === document.body) return { name: "body", changed: true };
      const name = `${element.tagName.toLowerCase()} "${(element.getAttribute("aria-label") ?? element.textContent ?? "").trim().slice(0, 40)}"`;
      // Arc draws focus on the control, or on what paints it: a highlight inside it, the field around it
      // (:focus-within) or the surface right after it (Add filter). Look at all of them.
      const painted = [element, element.parentElement, element.parentElement?.parentElement, element.nextElementSibling, ...Array.from(element.children).slice(0, 3)].filter((node): node is Element => !!node);
      const snapshot = () => painted.map(node => {
        const style = getComputedStyle(node);
        return [style.backgroundColor, style.borderColor, style.boxShadow, style.color, style.textDecorationLine, style.outlineStyle, style.transform, style.opacity].join("|");
      }).join("/");
      const now = snapshot();
      element.blur();
      const rest = snapshot();
      element.focus({ preventScroll: true });
      return { name, changed: now !== rest };
    });
    if (!visible.changed) unseen.push(visible.name);
  }
  expect(unseen, `focus not visible on: ${unseen.join(", ")}`).toEqual([]);
  await context.close();
});
