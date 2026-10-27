import { expect,test,type Page } from "@playwright/test";
import { accountsSession,runAccounts,sameOrigin,signIn,watchErrors } from "./support";
import { audit } from "./audit";
async function select(page:Page,range:string){await page.getByTestId("name-box").click();await page.getByTestId("name-box").fill(range);await page.getByTestId("name-box").press("Enter");await expect(page.getByTestId("name-box")).toHaveValue(range);}
test("conditional rules, scales, alternating rows and chips update and persist",async({browser})=>{
 const context=await accountsSession(browser,"carbon",{bypassCSP:true,reducedMotion:"reduce"});const page=await context.newPage();const errors=watchErrors(page);
 await signIn(page,runAccounts().carbon.email);await page.getByRole("button",{name:"New sheet",exact:true}).click();await expect(page.getByTestId("grid")).toBeVisible();const id=new URL(page.url()).pathname.split("/")[2];
 const edit=async(edits:unknown[])=>page.request.post(`/api/v1/sheets/${id}/edits`,{headers:sameOrigin(),data:{edits}});
 const cell=async(address:string)=>(await(await page.request.get(`/api/v1/sheets/${id}/tabs/1/cells?range=${address}`)).json()).cells[0];
 await edit([{type:"set",tab:1,at:"A1",rows:[["Item","Amount","Status"],["One","50"],["Two","150"],["Three","200"]]},{type:"set",tab:1,at:"E2",rows:[["someone@example.com","https://example.com/report.pdf"]]}]);
 await select(page,"A1:C4");await page.getByRole("button",{name:"Alternating colours",exact:true}).click();await audit(page,"alternating colours");await page.getByRole("button",{name:"Apply",exact:true}).click();
 await expect.poll(async()=>(await cell("A1"))?.cs?.fill).toBe("#E3E3E3");await expect.poll(async()=>(await cell("A3"))?.cs?.fill).toBe("#D5E5DA");
 await select(page,"B2:B4");await page.getByRole("button",{name:"Conditional formatting",exact:true}).click();await page.getByRole("textbox",{name:"Value",exact:true}).fill("100");await page.getByRole("button",{name:"Apply",exact:true}).click();
 await expect.poll(async()=>(await cell("B3"))?.cs?.fill).toBe("#E8EFE9");await expect.poll(async()=>(await cell("B2"))?.cs?.fill).toBe("#F7F7F7");
 await edit([{type:"set",tab:1,at:"B2",rows:[["120"]]}]);await expect.poll(async()=>(await cell("B2"))?.cs?.fill).toBe("#E8EFE9");
 await select(page,"C2:C4");await page.getByRole("button",{name:"Conditional formatting",exact:true}).click();await page.getByRole("combobox",{name:"Format cells if",exact:true}).click();await page.getByRole("option",{name:"Custom formula is true",exact:true}).click();await page.getByRole("textbox",{name:"Formula",exact:true}).fill("=$B2>180");await page.getByRole("button",{name:"Apply",exact:true}).click();await expect.poll(async()=>(await cell("C4"))?.cs?.fill).toBe("#E8EFE9");
 await select(page,"D2:D3");await page.getByRole("button",{name:"Cell controls",exact:true}).click();await page.getByRole("menuitem",{name:"Rating",exact:true}).click();await page.getByRole("radiogroup",{name:"Rating D2",exact:true}).getByRole("radio",{name:"3 stars",exact:true}).click();await expect.poll(async()=>(await cell("D2"))?.v).toEqual({t:"number",v:3});
 for(const [address,label] of [["E2","Person chip…"],["F2","File chip…"]]){await select(page,address!);await page.getByRole("button",{name:"Cell controls",exact:true}).click();await page.getByRole("menuitem",{name:label!,exact:true}).click();await page.getByRole("button",{name:"Apply",exact:true}).click();}
 await expect(page.getByRole("link",{name:/Open file F2/})).toHaveAttribute("href","https://example.com/report.pdf");await expect(page.getByText("someone@example.com",{exact:true})).toBeVisible();
 await page.reload();await expect(page.getByRole("radiogroup",{name:"Rating D2",exact:true}).getByRole("radio",{name:"3 stars",exact:true})).toHaveAttribute("aria-checked","true");await expect.poll(async()=>(await cell("C4"))?.cs?.fill).toBe("#E8EFE9");await audit(page,"conditional formatting and chips");await page.screenshot({path:"screens/conditional-formatting.png"});
 await select(page,"A1:C4");await page.getByRole("button",{name:"Alternating colours",exact:true}).click();await page.getByRole("button",{name:"Remove formatting rule A1:C4",exact:true}).click();await page.getByRole("button",{name:"Cancel",exact:true}).click();await expect.poll(async()=>(await cell("A1"))?.cs).toBeUndefined();
 expect(errors).toEqual([]);await context.close();
});
