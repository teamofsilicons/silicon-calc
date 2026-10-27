import { expect, test } from "@playwright/test";
import { accountsSession, runAccounts, sameOrigin, signIn, watchErrors } from "./support";

test("home includes owned and shared sheets while Shared with me remains a filtered view", async ({ browser }) => {
  const ownerContext = await accountsSession(browser, "carbon");
  const readerContext = await accountsSession(browser, "friend", { reducedMotion: "reduce" });
  const owner = await ownerContext.newPage();
  const reader = await readerContext.newPage();
  const errors = watchErrors(reader);
  const prefix = `Home listing ${Date.now()}`;
  const ownTitle = `${prefix} · My sheet`;
  const sharedTitle = `${prefix} · Shared sheet`;
  const privateTitle = `${prefix} · Private sheet`;
  const cleanup: Array<() => Promise<unknown>> = [];
  try {
    await signIn(owner, runAccounts().carbon.email);
    await signIn(reader, runAccounts().friend.email);
    const me = await (await reader.request.get("/api/v1/me")).json();
    const ownResponse = await reader.request.post("/api/v1/sheets", { headers: sameOrigin(), data: { title: ownTitle } });
    expect(ownResponse.ok()).toBeTruthy();
    const own = await ownResponse.json();
    cleanup.push(() => reader.request.delete(`/api/v1/sheets/${own.id}`, { headers: sameOrigin() }));
    const sharedResponse = await owner.request.post("/api/v1/sheets", { headers: sameOrigin(), data: { title: sharedTitle } });
    expect(sharedResponse.ok()).toBeTruthy();
    const shared = await sharedResponse.json();
    cleanup.push(() => owner.request.delete(`/api/v1/sheets/${shared.id}`, { headers: sameOrigin() }));
    const privateResponse = await owner.request.post("/api/v1/sheets", { headers: sameOrigin(), data: { title: privateTitle } });
    expect(privateResponse.ok()).toBeTruthy();
    const privateSheet = await privateResponse.json();
    cleanup.push(() => owner.request.delete(`/api/v1/sheets/${privateSheet.id}`, { headers: sameOrigin() }));
    const grant = await owner.request.post(`/api/v1/sheets/${shared.id}/sharing`, { headers: sameOrigin(), data: { invite: [me.account.id], role: "viewer", notify: false } });
    expect(grant.ok()).toBeTruthy();

    await reader.reload();
    await expect(reader.getByRole("link", { name: ownTitle, exact: true })).toBeVisible();
    await expect(reader.getByRole("link", { name: sharedTitle, exact: true })).toBeVisible();
    await expect(reader.getByRole("link", { name: privateTitle, exact: true })).toHaveCount(0);
    await expect(reader.getByRole("button", { name: "New sheet", exact: true })).toBeVisible();
    await reader.getByRole("searchbox", { name: "Search sheets and folders", exact: true }).fill(prefix);
    await expect(reader.getByRole("list", { name: "Sheets and folders" }).getByRole("listitem")).toHaveCount(2);
    await expect(reader.getByRole("listitem").filter({ has: reader.getByRole("link", { name: sharedTitle, exact: true }) })).toContainText("viewer");
    await reader.screenshot({ path: test.info().outputPath("shared-home.png") });
    await reader.getByRole("button", { name: `Actions for ${sharedTitle}`, exact: true }).click();
    await expect(reader.getByRole("menuitem", { name: "Sharing", exact: true })).toBeVisible();
    await expect(reader.getByRole("menuitem", { name: "Rename", exact: true })).toHaveCount(0);
    await expect(reader.getByRole("menuitem", { name: "Move to trash", exact: true })).toHaveCount(0);
    await reader.keyboard.press("Escape");

    await reader.getByRole("link", { name: "Shared with me", exact: true }).click();
    await expect(reader.getByRole("heading", { level: 1 })).toHaveText("Shared with me");
    await expect(reader.getByRole("link", { name: sharedTitle, exact: true })).toBeVisible();
    await expect(reader.getByRole("link", { name: ownTitle, exact: true })).toHaveCount(0);
    await reader.getByRole("link", { name: sharedTitle, exact: true }).click();
    await expect(reader.getByTestId("grid")).toBeVisible();
    await expect(reader.getByText("Viewing this sheet", { exact: true })).toBeVisible();

    const revoke = await owner.request.delete(`/api/v1/sheets/${shared.id}/sharing/${me.account.uuid}`, { headers: sameOrigin() });
    expect(revoke.ok()).toBeTruthy();
    await reader.goto("/sheets");
    await reader.getByRole("searchbox", { name: "Search sheets and folders", exact: true }).fill(prefix);
    await expect(reader.getByRole("link", { name: ownTitle, exact: true })).toBeVisible();
    await expect(reader.getByRole("link", { name: sharedTitle, exact: true })).toHaveCount(0);
    expect(errors).toEqual([]);
  } finally {
    for (const remove of cleanup) await remove();
    await ownerContext.close();
    await readerContext.close();
  }
});
