import assert from 'node:assert/strict';
const url=process.env.WEB_URL;
const before=await(await fetch(url)).json();assert.ok(before.some(x=>x.sku==='BOOK-1'));
const response=await fetch(url,{method:'POST',body:JSON.stringify({sku:' book-1 ',name:'duplicate'})});
assert.equal(response.status,409,'SKU uniqueness must ignore surrounding whitespace and case');
assert.deepEqual(await(await fetch(url)).json(),before,'rejected duplicate must not mutate storage');
console.log(JSON.stringify({ok:true,preserved:before}));
