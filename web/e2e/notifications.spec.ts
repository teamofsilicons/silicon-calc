import { expect, test } from "@playwright/test";
import { accountsSession, runAccounts, signIn } from "./support";
import { audit } from "./audit";

test("Ting notifications require an explicit choice and failed registration can be retried", async ({ browser }) => {
  const context = await accountsSession(browser, "carbon", { reducedMotion: "reduce", bypassCSP: true });
  const page = await context.newPage();
  let writes = 0;
  let disables = 0;
  let enabled = false;
  await page.route("**/api/v1/me/notifications", route => {
    if (route.request().method() === "POST") {
      writes++;
      if (writes === 1) return route.fulfill({ status: 503, contentType: "application/json", body: JSON.stringify({ error: { code: "notifications_unavailable", message: "Ting could not be reached.", hint: "Try again in a moment." } }) });
      enabled = true;
    }
    if (route.request().method() === "DELETE") { disables++; enabled = false; }
    return route.fulfill({ contentType: "application/json", body: JSON.stringify({ enabled }) });
  });
  await signIn(page, runAccounts().carbon.email);
  expect(writes).toBe(0);
  await page.getByRole("button", { name: /^Account menu, / }).click();
  await page.getByRole("menuitem", { name: "Notifications", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "Calc notifications", exact: true });
  await expect(dialog).toBeVisible();
  await expect(dialog).toContainText("mentions and sharing updates through Silicon Ting");
  await expect(dialog.getByRole("button", { name: "Enable Calc notifications", exact: true })).toBeEnabled();
  expect(writes).toBe(0);
  await audit(page, "Calc notifications before opt-in");
  await dialog.getByRole("button", { name: "Enable Calc notifications", exact: true }).click();
  await expect(dialog.getByRole("alert")).toContainText("Ting could not be reached. Try again in a moment.");
  expect(writes).toBe(1);
  await dialog.getByRole("button", { name: "Enable Calc notifications", exact: true }).click();
  await expect(dialog.getByRole("status")).toHaveText("Calc notifications are enabled for this account.");
  expect(writes).toBe(2);
  await expect(dialog.getByRole("button", { name: "Enable Calc notifications", exact: true })).toHaveCount(0);
  await dialog.getByRole("button", { name: "Close dialog" }).click();
  await page.getByRole("button", { name: /^Account menu, / }).click();
  await page.getByRole("menuitem", { name: "Notifications", exact: true }).click();
  await expect(dialog.getByRole("status")).toHaveText("Calc notifications are enabled for this account.");
  expect(writes).toBe(2);
  expect(disables).toBe(0);
  await dialog.getByRole("button", { name: "Disable Calc notifications", exact: true }).click();
  await expect(dialog.getByRole("button", { name: "Enable Calc notifications", exact: true })).toBeEnabled();
  expect(disables).toBe(1);
  await context.close();
});
