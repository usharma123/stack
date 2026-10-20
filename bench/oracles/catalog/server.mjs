import http from 'node:http';
import fs from 'node:fs';
const file='products.json';
const read=()=>JSON.parse(fs.readFileSync(file));
http.createServer(async(req,res)=>{
 res.setHeader('Content-Type','application/json');
 if(req.url==='/identity'){res.end(process.env.IDENTITY_TOKEN);return;}
 if(req.method==='GET'){res.end(JSON.stringify(read()));return;}
 let body='';for await(const part of req)body+=part;
 const product=JSON.parse(body), rows=read();
 product.sku=product.sku.trim().toUpperCase();
 if(rows.some(x=>x.sku===product.sku)){res.statusCode=409;res.end(JSON.stringify({error:'duplicate'}));return;}
 rows.push(product);fs.writeFileSync(file,JSON.stringify(rows));res.statusCode=201;res.end(JSON.stringify(product));
}).listen(Number(process.env.PORT),'127.0.0.1');
