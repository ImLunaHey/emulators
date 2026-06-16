// Cloudflare Worker entry point.
//
// Responsibilities:
//   1. Proxy /api/hasheous/* → https://hasheous.org/api/v1/* with CORS.
//   2. Serve /api/igdb/cover/<igdb_game_id> by looking up the IGDB
//      cover for that game ID and 302-redirecting to the actual image
//      URL on images.igdb.com. The Twitch OAuth token is cached
//      in-memory so subsequent requests don't re-auth.
//   3. Everything else falls through to the static assets binding.

export interface Env {
  ASSETS: Fetcher;
  TWITCH_CLIENT_ID?: string;
  TWITCH_CLIENT_SECRET?: string;
  // Durable Object namespace backing the link-cable signaling rooms.
  // One instance per room id (e.g. "ABC123"); see SignalRoom below.
  SIGNAL: DurableObjectNamespace;
}

const HASHEOUS_BASE = 'https://hasheous.org/api/v1';

export default {
  async fetch(req: Request, env: Env, _ctx: ExecutionContext): Promise<Response> {
    const url = new URL(req.url);

    if (url.pathname.startsWith('/api/hasheous/')) {
      return proxyHasheous(req, url);
    }
    if (url.pathname.startsWith('/api/igdb/cover/')) {
      return igdbCover(url, env);
    }
    if (url.pathname.startsWith('/api/signal/')) {
      // Route by room code → one DO per room. idFromName is
      // deterministic, so two clients passing the same code land on
      // the same DO instance no matter which colo each hits.
      const roomId = url.pathname.replace('/api/signal/', '');
      if (!roomId) return new Response('missing room id', { status: 400 });
      const id = env.SIGNAL.idFromName(roomId);
      const stub = env.SIGNAL.get(id);
      return stub.fetch(req);
    }

    return env.ASSETS.fetch(req);
  },
};

// ---------------------------------------------------------------- Signaling DO
//
// SignalRoom holds the WebSocket connections of every peer in a single
// link-cable room and relays WebRTC signaling messages (SDP offers,
// answers, ICE candidates) between them. Two clients connect via
// `wss://<host>/api/signal/<roomId>`, exchange SDP and ICE through us,
// then talk peer-to-peer over an RTCDataChannel — we drop out of the
// loop once their direct path is up.
//
// The DO is intentionally trivial:
//   - assign each connection a random peerId,
//   - tell the new peer who's already in the room (their peerIds),
//   - tell existing peers that a new peer joined,
//   - forward any `{to: peerId, ...}` message verbatim to that peer,
//   - tell remaining peers when someone leaves.
// No persistence — if the DO evicts between sessions, it just spawns
// fresh on next connect.

interface SignalMessage {
  // self / peer-join / peer-leave are server-emitted (room control).
  // state is client-to-client SIO state relay (no WebRTC). The room
  // doesn't introspect the type; it just forwards anything with a
  // `to` field to that peer.
  type: 'self' | 'peer-join' | 'peer-leave' | 'state';
  to?: string;
  from?: string;
  peerId?: string;
  peers?: string[];
  payload?: unknown;
}

export class SignalRoom {
  // Live WebSockets keyed by peerId.
  private peers = new Map<string, WebSocket>();

  // DurableObjectState is provided but unused — we don't persist state
  // across evictions. The `_state` underscore tells the linter so.
  // eslint-disable-next-line no-unused-vars, @typescript-eslint/no-unused-vars
  constructor(_state: DurableObjectState, _env: Env) {}

  async fetch(req: Request): Promise<Response> {
    if (req.headers.get('Upgrade') !== 'websocket') {
      return new Response('expected WebSocket', { status: 426 });
    }
    const pair = new WebSocketPair();
    const client = pair[0];
    const server = pair[1];
    server.accept();

    const peerId = crypto.randomUUID();
    this.peers.set(peerId, server);

    // Tell the new peer their id + who's already here.
    const others = [...this.peers.keys()].filter((id) => id !== peerId);
    this.sendTo(server, { type: 'self', peerId, peers: others });
    // Tell everyone else there's a new peer.
    for (const [id, ws] of this.peers) {
      if (id !== peerId) this.sendTo(ws, { type: 'peer-join', peerId });
    }

    server.addEventListener('message', (ev) => {
      let msg: SignalMessage;
      try { msg = JSON.parse(ev.data as string); } catch { return; }
      // We only forward addressed messages; broadcasts aren't needed
      // for 1:1 signaling and would expand the attack surface for
      // bad clients spamming the room.
      if (msg.to && this.peers.has(msg.to)) {
        msg.from = peerId;
        this.sendTo(this.peers.get(msg.to)!, msg);
      }
    });

    const cleanup = () => {
      if (!this.peers.has(peerId)) return;
      this.peers.delete(peerId);
      for (const [, ws] of this.peers) {
        this.sendTo(ws, { type: 'peer-leave', peerId });
      }
    };
    server.addEventListener('close', cleanup);
    server.addEventListener('error', cleanup);

    return new Response(null, { status: 101, webSocket: client });
  }

  private sendTo(ws: WebSocket, msg: SignalMessage): void {
    try { ws.send(JSON.stringify(msg)); } catch { /* socket may be closing */ }
  }
}

// ---------------------------------------------------------------- Hasheous

async function proxyHasheous(req: Request, url: URL): Promise<Response> {
  const rest = url.pathname.replace(/^\/api\/hasheous/, '');
  const upstreamPath = rest
    .replace(/^\/lookup\/byhash\//i, '/Lookup/ByHash/')
    .replace(/^\/healthcheck/i, '/Healthcheck')
    .replace(/^\/lookup\/platforms/i, '/Lookup/Platforms');
  const upstream = HASHEOUS_BASE + upstreamPath + url.search;

  if (req.method === 'OPTIONS') {
    return new Response(null, { status: 204, headers: corsHeaders('GET, OPTIONS') });
  }
  if (req.method !== 'GET') {
    return new Response('method not allowed', { status: 405, headers: corsHeaders('GET, OPTIONS') });
  }

  const upstreamResp = await fetch(upstream, {
    method: 'GET',
    headers: { 'Accept': 'application/json' },
    cf: { cacheTtl: 86400, cacheEverything: true },
  });

  const headers = new Headers(upstreamResp.headers);
  for (const [k, v] of Object.entries(corsHeaders('GET, OPTIONS'))) headers.set(k, v);
  headers.delete('alt-svc');
  headers.delete('server');
  return new Response(upstreamResp.body, {
    status: upstreamResp.status,
    statusText: upstreamResp.statusText,
    headers,
  });
}

// ---------------------------------------------------------------- IGDB

// Worker-local cache for the Twitch app-access token. Twitch tokens
// last ~60 days; we refresh just before expiry. This cache lives in
// the Worker isolate so a single Worker handles many requests with
// one token, but cold isolates pay one extra Twitch round-trip.
let cachedToken: { value: string; expiresAt: number } | null = null;

async function getTwitchToken(env: Env): Promise<string> {
  if (cachedToken && cachedToken.expiresAt > Date.now() + 60_000) {
    return cachedToken.value;
  }
  if (!env.TWITCH_CLIENT_ID || !env.TWITCH_CLIENT_SECRET) {
    throw new Error('TWITCH_CLIENT_ID / TWITCH_CLIENT_SECRET not configured');
  }
  const body = new URLSearchParams({
    client_id: env.TWITCH_CLIENT_ID,
    client_secret: env.TWITCH_CLIENT_SECRET,
    grant_type: 'client_credentials',
  });
  const resp = await fetch('https://id.twitch.tv/oauth2/token', {
    method: 'POST',
    body,
    headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
  });
  if (!resp.ok) throw new Error(`Twitch token request failed: ${resp.status}`);
  const json = await resp.json() as { access_token: string; expires_in: number };
  cachedToken = {
    value: json.access_token,
    expiresAt: Date.now() + json.expires_in * 1000,
  };
  return cachedToken.value;
}

async function igdbCover(url: URL, env: Env): Promise<Response> {
  const idStr = url.pathname.replace(/^\/api\/igdb\/cover\//, '');
  const igdbId = parseInt(idStr, 10);
  if (!Number.isFinite(igdbId) || igdbId <= 0) {
    return new Response('bad game id', { status: 400, headers: corsHeaders('GET, OPTIONS') });
  }
  let token: string;
  try {
    token = await getTwitchToken(env);
  } catch (e) {
    return new Response((e as Error).message, { status: 500, headers: corsHeaders('GET, OPTIONS') });
  }

  // IGDB's covers endpoint — POST body in their custom Apicalypse DSL.
  // Returns at most one cover (we limit to 1) with the image_id we need.
  const body = `where game = ${igdbId}; fields image_id; limit 1;`;
  const igdbResp = await fetch('https://api.igdb.com/v4/covers', {
    method: 'POST',
    body,
    headers: {
      'Authorization': `Bearer ${token}`,
      'Client-ID': env.TWITCH_CLIENT_ID!,
      'Accept': 'application/json',
      'Content-Type': 'text/plain',
    },
  });
  if (!igdbResp.ok) {
    return new Response('igdb cover lookup failed', {
      status: igdbResp.status,
      headers: corsHeaders('GET, OPTIONS'),
    });
  }
  const covers = await igdbResp.json() as Array<{ image_id?: string }>;
  if (!covers.length || !covers[0].image_id) {
    return new Response('no cover', { status: 404, headers: corsHeaders('GET, OPTIONS') });
  }
  const imageUrl = `https://images.igdb.com/igdb/image/upload/t_cover_big/${covers[0].image_id}.jpg`;
  // Stream the image bytes back through the worker so the browser
  // sees same-origin pixels (no CORS, no referrer leak to igdb.com)
  // and we can edge-cache the image too.
  const imageResp = await fetch(imageUrl, {
    cf: { cacheTtl: 7 * 24 * 3600, cacheEverything: true },
  });
  const headers = new Headers(imageResp.headers);
  for (const [k, v] of Object.entries(corsHeaders('GET, OPTIONS'))) headers.set(k, v);
  headers.set('Cache-Control', 'public, max-age=604800');
  return new Response(imageResp.body, {
    status: imageResp.status,
    statusText: imageResp.statusText,
    headers,
  });
}

// ---------------------------------------------------------------- CORS

function corsHeaders(allowMethods: string): Record<string, string> {
  return {
    'Access-Control-Allow-Origin': '*',
    'Access-Control-Allow-Methods': allowMethods,
    'Access-Control-Allow-Headers': 'Content-Type, Accept',
    'Access-Control-Max-Age': '86400',
  };
}
