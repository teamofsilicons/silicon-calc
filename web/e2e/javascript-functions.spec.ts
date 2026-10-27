import { expect, test, type Page } from "@playwright/test";
import { accountsSession, runAccounts, sameOrigin, signIn, watchErrors } from "./support";
import { audit } from "./audit";
async function choose(page: Page, mode: string) { await page.getByRole("combobox", { name:"JavaScript", exact:true }).click(); await page.getByRole("option", { name:mode, exact:true }).click(); }
test("sheet and account functions, inline JS, dependent formulas, persistence and library reuse", async ({browser}) => {
  const context=await accountsSession(browser,"carbon",{reducedMotion:"reduce"});
  const page=await context.newPage(); const errors=watchErrors(page);
  await signIn(page,runAccounts().carbon.email);
  await page.getByRole("button",{name:"New sheet",exact:true}).click(); await expect(page.getByTestId("grid")).toBeVisible();
  const path=new URL(page.url()).pathname;const id=path.split("/")[2];
  const values=async(range="A1:F3") => (await (await page.request.get(`/api/v1/sheets/${id}/tabs/1/cells?range=${range}&format=values`)).json()).values;
  await page.getByRole("button",{name:"Code",exact:true}).click(); await choose(page,"Functions");
  // A slow frame load must not consume a formula's five-second execution limit.
  await page.route("**/code-sandbox", async route => {
    await new Promise(resolve => setTimeout(resolve, 6000));
    await route.continue();
  }, { times: 1 });
  await page.getByRole("button",{name:"Save function",exact:true}).click();
  await expect(page.getByRole("status").filter({hasText:"Saved. Use =DOUBLE_VALUE"})).toBeVisible({timeout:15000});
  // A completed worker result can be delivered late when Chrome suspends the
  // isolated frame. Transport delay must not be persisted as a code timeout.
  await page.route("**/code-sandbox", async route => {
    const response = await route.fetch();
    const body = (await response.text()).replace(
      'const send = data => parent.postMessage({ ...data, nonce }, "*");',
      'const send = data => { const deliver = () => parent.postMessage({ ...data, nonce }, "*"); data?.kind === "result" ? setTimeout(deliver, 6500) : deliver(); };',
    );
    await route.fulfill({ response, body });
  }, { times: 1 });
  await page.request.post(`/api/v1/sheets/${id}/edits`,{headers:sameOrigin(),data:{edits:[{type:"set",tab:1,at:"A1",rows:[["5","=DOUBLE_VALUE(A1)","=B1+2",'=JS("return args[0] * 3;", A1)','=JS("return [[1,2],[3,4]];")']]}]}});
  await expect.poll(() => values("A1:F2"),{timeout:20000}).toEqual([[5,10,12,15,1,2],[null,null,null,null,3,4]]);
  await page.request.post(`/api/v1/sheets/${id}/edits`,{headers:sameOrigin(),data:{edits:[{type:"set",tab:1,at:"A1",rows:[["7"]]}]}});
  await expect.poll(() => values("A1:D1")).toEqual([[7,14,16,21]]);
  await page.getByRole("textbox",{name:"Function name",exact:true}).fill("CALC_TEST_TOTAL");
  await page.getByRole("textbox",{name:"Parameters, separated by commas",exact:true}).fill("values");
  await page.getByRole("textbox",{name:"JavaScript function body",exact:true}).fill('return [values].flat(Infinity).filter(x => typeof x === "number").reduce((sum,x) => sum+x, 0);');
  await page.getByRole("combobox",{name:"Available in",exact:true}).click();await page.getByRole("option",{name:"All my sheets",exact:true}).click();
  await page.getByRole("button",{name:"Save function",exact:true}).click();await expect(page.getByRole("status").filter({hasText:"Saved. Use =CALC_TEST_TOTAL"})).toBeVisible();
  await page.reload();await expect.poll(() => values("A1:D1")).toEqual([[7,14,16,21]]);
  await page.getByRole("button",{name:"Code",exact:true}).click();await choose(page,"JavaScript cell");
  await page.getByTestId("name-box").fill("A3");await page.getByTestId("name-box").press("Enter");
  await page.getByRole("textbox",{name:"JavaScript cell body",exact:true}).fill('return (await calc.cell("A1")) + 100;');
  await page.getByRole("button",{name:"Apply to cell",exact:true}).click();await expect.poll(() => values("A3")).toEqual([[107]]);
  await page.screenshot({path:"screens/javascript-cell.png"});
  const created=await page.request.post("/api/v1/sheets",{headers:sameOrigin(),data:{title:"Library reuse"}});const second=(await created.json()).id;
  await page.request.post(`/api/v1/sheets/${second}/edits`,{headers:sameOrigin(),data:{edits:[{type:"set",tab:1,at:"A1",rows:[["2","=CALC_TEST_TOTAL(A1:A2)"],["3"]]}]}});
  const helper = await page.request.put("/api/v1/me/functions/CALC_TEST_HELPER", {headers:sameOrigin(), data:{name:"CALC_TEST_HELPER",parameters:["values"],code:"return (await CALC_TEST_TOTAL(values)) * 2;"}});
  expect(helper.ok()).toBeTruthy();
  await page.request.post(`/api/v1/sheets/${second}/edits`,{headers:sameOrigin(),data:{edits:[{type:"set",tab:1,at:"C1",rows:[["=CALC_TEST_HELPER(A1:A2)"]]}]}});
  await page.goto(`/s/${second}`);
  await expect.poll(async()=> (await(await page.request.get(`/api/v1/sheets/${second}/tabs/1/cells?range=B1&format=values`)).json()).values).toEqual([[5]]);
  await expect.poll(async()=> (await(await page.request.get(`/api/v1/sheets/${second}/tabs/1/cells?range=C1&format=values`)).json()).values).toEqual([[10]]);
  const guide=await context.newPage();await guide.goto("/docs/javascript");await expect(guide.getByRole("heading",{name:"JavaScript in Calc",exact:true})).toBeVisible();await expect(guide.locator("main")).toContainText("calc.values");
  expect(errors).toEqual([]);await context.close();
});

test("JavaScript errors and timeouts are visible, and values are literal",async({browser})=>{
  const context=await accountsSession(browser,"carbon",{reducedMotion:"reduce",bypassCSP:true});const page=await context.newPage();await signIn(page,runAccounts().carbon.email);await page.getByRole("button",{name:"New sheet",exact:true}).click();await expect(page.getByTestId("grid")).toBeVisible();const id=new URL(page.url()).pathname.split("/")[2];
  await page.request.post(`/api/v1/sheets/${id}/edits`,{headers:sameOrigin(),data:{edits:[{type:"set",tab:1,at:"A1",rows:[['=JS("return \'=2+2\';")','=JS("throw new Error(\'Test error\');")','=JS("while (true) {}")']]}]}});
  await expect.poll(async()=> (await(await page.request.get(`/api/v1/sheets/${id}/tabs/1/cells?range=A1:C1&format=values`)).json()).values,{timeout:20000}).toEqual([["=2+2","#CALC!","#CALC!"]]);
  await page.getByRole("button",{name:"Code",exact:true}).click();await choose(page,"Functions");await expect(page.getByRole("region",{name:"JavaScript errors"})).toContainText("Test error");await expect(page.getByRole("region",{name:"JavaScript errors"})).toContainText("exceeded 5 seconds");
  await audit(page,"JavaScript functions");await context.close();
});
