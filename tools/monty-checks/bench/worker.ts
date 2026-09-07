import {DurableObject} from 'cloudflare:workers';

export class Counter extends DurableObject {
  ping() { return this.ctx.blockConcurrencyWhile(() => 1); }
  increment() {
    return this.ctx.blockConcurrencyWhile(() => this.ctx.storage.transactionSync(() => {
      const value = (this.ctx.storage.kv.get('count') ?? 0) + 1;
      this.ctx.storage.kv.put('count', value);
      return value;
    }));
  }
}

export default {async fetch(request, env) {
  const path = new URL(request.url).pathname;
  if (!['/hello','/echo','/increment','/pause','/object','/fetch'].includes(path)) return new Response('unknown handler',{status:404});
  if (request.method !== 'POST') return new Response('POST required',{status:405});
  const args = await request.json();
  switch (path) {
    case '/hello': return new Response(`Hello, ${args.name ?? 'world'}!`, {headers:{'content-type':'text/plain; charset=utf-8'}});
    case '/echo': return Response.json({value:args.value});
    case '/increment': return Response.json({value:await env.COUNTER.getByName(args.id).increment()});
    case '/object': return Response.json({value:await env.COUNTER.getByName(args.id).ping()});
    case '/fetch': return await fetch(args.url, {method:'POST', body:JSON.stringify({value:'x'.repeat(16384)})});
    case '/pause':
      await new Promise(resolve => setTimeout(resolve,10));
      return new Response('done',{headers:{'content-type':'text/plain; charset=utf-8'}});
  }
}};
