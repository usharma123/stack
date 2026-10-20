import http from 'node:http';
const loaded=await(await fetch(process.env.WEB_URL+'/version')).json();
http.createServer((req,res)=>res.end(req.url==='/identity'?process.env.STACK_IDENTITY_WORKER:JSON.stringify(loaded))).listen(Number(process.env.PORT),'127.0.0.1');
