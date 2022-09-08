import { serve } from 'https://deno.land/std@0.154.0/http/server.ts';

const port = 80;

const handler = (request: Request): Response => {
  if (request.method === "DELETE") {
    setTimeout(() => Deno.exit(0), 1000);
  }

  console.log(request.method);

  return new Response(request.method, { status: 200 });
};

await serve(handler, { port });
