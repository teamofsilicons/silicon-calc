import { expect, test } from "@playwright/test";
import { accountsSession, runAccounts, sameOrigin, signIn, BASE } from "./support";
import { audit } from "./audit";

test("public email collection remembers guests and gives only the owner viewer history", async ({ browser }) => {
  const owner = await accountsSession(browser, "carbon", { bypassCSP: true, reducedMotion: "reduce" });
  const page = await owner.newPage();
  await signIn(page, runAccounts().carbon.email);
  const created = await page.request.post("/api/v1/sheets", { headers: sameOrigin(), data: { title: "Email collection test" } });
  expect(created.ok()).toBeTruthy();
  const { id } = await created.json() as { id: string };
  const base = `/api/v1/sheets/${id}`;
  const policy = async (require_email: boolean) => {
    expect((await page.request.put(`${base}/public`, { headers: sameOrigin(), data: { enabled: true, access: "view", require_email } })).ok()).toBeTruthy();
  };
  await policy(false);
  await page.goto(`/s/${id}`);
  await page.getByRole("button", { name: "Share", exact: true }).click();
  const dialog = page.getByRole("dialog");
  await dialog.getByRole("checkbox", { name: "Require email to view" }).check();
  await expect(dialog.getByRole("checkbox", { name: "Require email to view" })).toBeChecked();
  await expect(dialog.getByRole("checkbox", { name: "Require email to view" })).toBeEnabled();

  const anon = await browser.newContext({ baseURL: BASE, bypassCSP: true, reducedMotion: "reduce" });
  const guest = await anon.newPage();
  await guest.goto(`/s/${id}`);
  await expect(guest.getByRole("heading", { name: "Enter your email to view" })).toBeVisible();
  await expect(guest.getByTestId("grid")).toHaveCount(0);
  expect(await guest.content()).not.toContain("Email collection test");
  const apiBase = process.env.CALC_API_URL || "http://127.0.0.1:8730";
  for (const suffix of ["", "/tabs/1/cells?range=A1", "/download?format=csv", "/events"]) {
    const response = await anon.request.get(`${apiBase}/v1/sheets/${id}${suffix}`);
    expect(response.status()).toBe(403);
    expect((await response.json()).error.code).toBe("email_required");
  }
  await audit(guest, "Public email prompt");
  expect((await guest.request.post(`${base}/visits`, { headers: sameOrigin(), data: { email: "bad" } })).status()).toBe(422);
  await guest.getByRole("textbox", { name: "Email", exact: true }).fill("guest@example.com");
  await guest.getByRole("button", { name: "View file", exact: true }).click();
  await expect(guest.getByTestId("grid")).toBeVisible();
  const grant = (await anon.cookies()).find(cookie => cookie.name === `calc-view-${id}`)!;
  expect(grant.httpOnly).toBe(true);
  expect(await guest.evaluate(() => document.cookie)).not.toContain(grant.value);
  await guest.reload();
  await expect(guest.getByTestId("grid")).toBeVisible();
  expect((await guest.request.get(`${base}/viewers`)).status()).toBe(401);
  expect((await guest.request.post(`${base}/edits`, { headers: sameOrigin(), data: { edits: [] } })).status()).toBe(401);

  const friend = await accountsSession(browser, "friend");
  const signed = await friend.newPage();
  await signed.goto("/sheets");
  await signed.getByRole("button", { name: /^Continue as / }).click();
  await signed.getByRole("checkbox", { name: /Email address/ }).check();
  await signed.getByRole("button", { name: "Share and continue", exact: true }).click();
  await expect(signed).toHaveURL(/\/sheets$/);
  await signed.goto(`/s/${id}`);
  await expect(signed.getByTestId("grid")).toBeVisible();
  await expect(signed.getByRole("heading", { name: "Enter your email to view" })).toHaveCount(0);
  expect((await signed.request.get(`${base}/viewers`)).status()).toBe(403);
  const viewers = await (await page.request.get(`${base}/viewers`)).json();
  expect(viewers.total).toBe(2);
  expect(viewers.viewers).toEqual(expect.arrayContaining([
    expect.objectContaining({ email: "guest@example.com", email_source: "entered" }),
    expect.objectContaining({ email: runAccounts().friend.email, email_source: "account" }),
  ]));
  await dialog.locator("summary").filter({ hasText: /Viewers/ }).click();
  await expect(dialog.getByText("guest@example.com", { exact: true })).toBeVisible();
  await expect(dialog.getByText("Entered email · unverified", { exact: true })).toBeVisible();
  await audit(page, "Owner viewer history");
  await dialog.getByText("guest@example.com", { exact: true }).scrollIntoViewIfNeeded();
  await page.screenshot({ path: "../.dev/production/2026-10-24-public-email/local-viewers.png" });
  await dialog.getByRole("checkbox", { name: "Require email to view" }).uncheck();
  await expect(dialog.getByRole("checkbox", { name: "Require email to view" })).toBeEnabled();
  const fresh = await browser.newContext({ baseURL: BASE, bypassCSP: true, reducedMotion: "reduce" });
  const freshPage = await fresh.newPage();
  await freshPage.goto(`/s/${id}`);
  await expect(freshPage.getByTestId("grid")).toBeVisible();
  await policy(true);
  await expect(freshPage.getByRole("heading", { name: "Enter your email to view" })).toBeVisible();
  await guest.reload();
  await expect(guest.getByRole("heading", { name: "Enter your email to view" })).toBeVisible();
  await guest.screenshot({ path: "../.dev/production/2026-10-24-public-email/local-prompt.png" });
  await fresh.close(); await friend.close(); await anon.close(); await owner.close();
});
