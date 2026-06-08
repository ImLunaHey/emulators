// Cloudflare Worker entry point.
//
// Two responsibilities:
//   1. Proxy /api/hasheous/* → https://hasheous.org/api/v1/* and add the
//      CORS header the upstream omits, so the SPA can call it from the
//      browser. Hasheous responds 200 + JSON but no Access-Control-Allow-
//      Origin, which the browser then strips from JS.
//   2. Let everything else fall through to the static assets binding
//      (the Vite build output). The wrangler config has
//      not_found_handling: single-page-application so unknown asset
//      paths serve index.html for client-side routing.

export interface Env {
  ASSETS: Fetcher;
}

const HASHEOUS_BASE = 'https://hasheous.org/api/v1';

export default {
  async fetch(req: Request, env: Env, _ctx: ExecutionContext): Promise<Response> {
    const url = new URL(req.url);

    if (url.pathname.startsWith('/api/hasheous/')) {
      return proxyHasheous(req, url);
    }

    return env.ASSETS.fetch(req);
  },
};

async function proxyHasheous(req: Request, url: URL): Promise<Response> {
  // /api/hasheous/lookup/md5/<hash>  →  /api/v1/Lookup/ByHash/md5/<hash>
  // Strip the /api/hasheous prefix and re-cased path the way Hasheous
  // expects (their paths are PascalCase).
  const rest = url.pathname.replace(/^\/api\/hasheous/, '');
  // Map the user-facing kebab path back to Hasheous's casing.
  const upstreamPath = rest
    .replace(/^\/lookup\/byhash\//i, '/Lookup/ByHash/')
    .replace(/^\/healthcheck/i, '/Healthcheck')
    .replace(/^\/lookup\/platforms/i, '/Lookup/Platforms');
  const upstream = HASHEOUS_BASE + upstreamPath + url.search;

  if (req.method === 'OPTIONS') {
    return new Response(null, {
      status: 204,
      headers: corsHeaders('GET, OPTIONS'),
    });
  }
  if (req.method !== 'GET') {
    return new Response('method not allowed', {
      status: 405,
      headers: corsHeaders('GET, OPTIONS'),
    });
  }

  const upstreamResp = await fetch(upstream, {
    method: 'GET',
    headers: { 'Accept': 'application/json' },
    // Cache responses at the edge — Hasheous metadata for a given hash
    // is effectively static, save the round-trip for repeat lookups.
    cf: { cacheTtl: 86400, cacheEverything: true },
  });

  const headers = new Headers(upstreamResp.headers);
  for (const [k, v] of Object.entries(corsHeaders('GET, OPTIONS'))) headers.set(k, v);
  // Drop hop-by-hop bits that don't apply to the re-emitted response.
  headers.delete('alt-svc');
  headers.delete('server');

  return new Response(upstreamResp.body, {
    status: upstreamResp.status,
    statusText: upstreamResp.statusText,
    headers,
  });
}

function corsHeaders(allowMethods: string): Record<string, string> {
  return {
    'Access-Control-Allow-Origin': '*',
    'Access-Control-Allow-Methods': allowMethods,
    'Access-Control-Allow-Headers': 'Content-Type, Accept',
    'Access-Control-Max-Age': '86400',
  };
}
