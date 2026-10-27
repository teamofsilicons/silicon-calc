import {test,expect} from '@playwright/test';
import {accountsSession,runAccounts,sameOrigin,signIn} from './support';
test('two open editors recalculate four JavaScript cells after banding and checkbox edits',async({browser})=>{
 const context=await accountsSession(browser,'carbon',{reducedMotion:'reduce'});const page=await context.newPage();await signIn(page,runAccounts().carbon.email);await page.getByRole('button',{name:'New sheet',exact:true}).click();await expect(page.getByTestId('grid')).toBeVisible();const path=new URL(page.url()).pathname,id=path.split('/')[2],base=`/api/v1/sheets/${id}`;
 const state=await(await page.request.get(base+'/functions')).json();
 expect((await page.request.put(base+'/functions',{headers:sameOrigin(),data:{base_version:state.version,function:{name:'CONCURRENT_DOUBLE',parameters:['value'],code:'return value*2;'}}})).ok()).toBeTruthy();
 const library=await page.request.put('/api/v1/me/functions/CONCURRENT_TEN',{headers:sameOrigin(),data:{name:'CONCURRENT_TEN',parameters:['value'],code:'return value+10;'}});expect(library.ok()).toBeTruthy();
 const second=await context.newPage();await second.goto(path);await expect(second.getByTestId('grid')).toBeVisible();
 const edit=async(edits:unknown[])=>{const response=await page.request.post(base+'/edits',{headers:sameOrigin(),data:{edits}});expect(response.ok()).toBeTruthy();};
 await edit([{type:'set',tab:1,at:'A1',rows:[['=JS("return 2+2;")','5','=CONCURRENT_DOUBLE(B1)','=JS("return args[0]+10;",B1)','=CONCURRENT_TEN(B1)']]},{type:'style',tab:1,range:'F1',patch:{control:{kind:'checkbox'}}},{type:'format_rule',tab:1,range:'A1:F4',rule:{kind:'alternating',first:'#FFFFFF',second:'#EEEEEE',header:null,footer:null}}]);
 const values=async()=>(await(await page.request.get(base+'/tabs/1/cells?range=A1:F1&format=values')).json()).values;
 await expect.poll(values,{timeout:45000}).toEqual([[4,5,10,15,15,false]]);
 await edit([{type:'set',tab:1,at:'B1',rows:[['7']]}]);
 await expect.poll(values,{timeout:45000}).toEqual([[4,7,14,17,17,false]]);
 await page.reload();await expect.poll(values).toEqual([[4,7,14,17,17,false]]);
 await context.close();
});

test('unrelated edits and formatting never restart cached JavaScript; real reads still recalculate',async({browser})=>{
 const context=await accountsSession(browser,'carbon',{reducedMotion:'reduce'});const page=await context.newPage();
 await signIn(page,runAccounts().carbon.email);await page.getByRole('button',{name:'New sheet',exact:true}).click();await expect(page.getByTestId('grid')).toBeVisible();
 const id=new URL(page.url()).pathname.split('/')[2],base=`/api/v1/sheets/${id}`;
 const state=await(await page.request.get(base+'/functions')).json();
 expect((await page.request.put(base+'/functions',{headers:sameOrigin(),data:{base_version:state.version,function:{name:'CACHED_DOUBLE',parameters:['value'],code:'return value*2;'}}})).ok()).toBeTruthy();
 const edit=async(edits:unknown[])=>{const response=await page.request.post(base+'/edits',{headers:sameOrigin(),data:{edits}});expect(response.ok()).toBeTruthy();};
 const values=async()=>(await(await page.request.get(base+'/tabs/1/cells?range=A1:F1&format=values')).json()).values;
 await edit([{type:'set',tab:1,at:'A1',rows:[['5','=CACHED_DOUBLE(A1)','=JS("return 4;")',`=JS("return (await calc.cell('A1'))*3;")`,`=JS("return (await calc.values('B1'))[0][0]+1;")`]]},{type:'style',tab:1,range:'F1',patch:{control:{kind:'checkbox'}}}]);
 await expect.poll(values,{timeout:45000}).toEqual([[5,10,4,15,11,false]]);
 // Stop all new workers: even a brief invalidation would now remain visibly BUSY.
 let restarts=0;
 await page.route('**/code-sandbox',async route=>{restarts++;await route.abort();});
 await edit([{type:'set',tab:1,at:'D10',rows:[['3']]}]);
 expect(await values()).toEqual([[5,10,4,15,11,false]]);
 await edit([{type:'style',tab:1,range:'A1:F1',patch:{bold:true}},{type:'format_rule',tab:1,range:'A1:F4',rule:{kind:'alternating',first:'#FFFFFF',second:'#EEEEEE',header:null,footer:null}}]);
 expect(await values()).toEqual([[5,10,4,15,11,false]]);
 await edit([{type:'set',tab:1,at:'F1',rows:[['TRUE']]}]);
 expect(await values()).toEqual([[5,10,4,15,11,true]]);
 await page.reload();await expect(page.getByTestId('grid')).toBeVisible();
 expect(await values()).toEqual([[5,10,4,15,11,true]]);expect(restarts).toBe(0);
 await page.unroute('**/code-sandbox');
 await edit([{type:'set',tab:1,at:'A1',rows:[['7']]}]);
 await expect.poll(values,{timeout:45000}).toEqual([[7,14,4,21,15,true]]);
 await context.close();
});
