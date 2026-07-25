# benny-rs Dashboard API — connection guide

This is the hand-off doc for wiring a web dashboard to the bot's `/api/v1`. The
full contract is in [`openapi.yaml`](./openapi.yaml).

That spec is mirrored byte-for-byte as `openapi/benny-api.yaml` in the
benny-dashboard repo, which generates its TypeScript client from it
(`bun run gen:api`). **Change both copies together** — a drift there is
invisible until a request 400s at runtime.

## Architecture (read this first)

```
Browser ──▶ Dashboard server (Next.js BFF) ──▶ benny-rs /api/v1 ──▶ SQLite + caches
           (holds the API token,                (bot owns the DB and the
            forwards X-Actor-Id)                 authoritative in-memory caches)
```

- The bot owns the databases **and** seven authoritative in-memory caches. To
  avoid stale-cache bugs, the dashboard never touches the DB directly — it calls
  this API, and every write updates the DB and the matching cache together.
- **The browser must never call `/api/v1` directly.** Only the dashboard's
  server-side code holds the bearer token. This is the Backend-for-Frontend
  (BFF) pattern: the browser talks to the Next.js server, which talks to the
  bot.
- Auth on every `/api/v1` request:
  - `Authorization: Bearer <dashboard_api_token>` — compared in constant time.
  - `X-Actor-Id: <discord user id>` — the acting user, used for the blacklist
    check and audit logging.

## 1. Configure the bot

The API is **off by default**. It is mounted only when `dashboard_api_token` is
set; otherwise every `/api/v1/*` path returns `404`.

Generate a token:

```bash
openssl rand -hex 32
```

Add to the bot's `config.json` (in the bot's working directory):

```json
{
  "token": "…your discord bot token…",
  "dashboard_api_token": "PASTE_THE_OPENSSL_OUTPUT_HERE",
  "dashboard_allowed_origin": "https://dashboard.your-domain.com"
}
```

- `dashboard_api_token` (string) — enables `/api/v1`. Keep it secret; rotate by
  changing it and restarting the bot.
- `dashboard_allowed_origin` (string, optional) — CORS allowlist for browsers.
  Because the dashboard uses a server-side BFF, the browser never hits the API
  directly, so this is defense-in-depth; set it to your dashboard origin or omit
  it. When omitted, no cross-origin browser requests are permitted.

The HTTP server binds to `127.0.0.1:8080` (loopback only). It is **not** exposed
to the internet on its own — put it behind a tunnel or reverse proxy (below).

Hardening that is always on for `/api/v1`: constant-time token compare, per-actor
rate limiting (100 requests / 10s), a 64 KiB request-body cap, a 15s per-request
timeout, and a structured audit log line for every mutation.

## 2. Expose the API to the dashboard server

The bot listens on loopback only, so you need one hop to reach it from your
dashboard host. Pick **one** of the following.

### Option A — Cloudflare Tunnel (`cloudflared`)

No open inbound ports; the tunnel dials out to Cloudflare.

```bash
# On the bot host:
cloudflared tunnel login
cloudflared tunnel create benny-api
```

`~/.cloudflared/config.yml`:

```yaml
tunnel: benny-api
credentials-file: /root/.cloudflared/<TUNNEL_ID>.json
ingress:
  - hostname: api.your-domain.com
    service: http://127.0.0.1:8080
  - service: http_status:404
```

```bash
cloudflared tunnel route dns benny-api api.your-domain.com
cloudflared tunnel run benny-api      # or install as a service: cloudflared service install
```

Your dashboard then calls `https://api.your-domain.com/api/v1/...`.

### Option B — Caddy reverse proxy (automatic HTTPS)

Point an `A`/`AAAA` record for `api.your-domain.com` at the host, open 80/443,
and let Caddy handle TLS.

`/etc/caddy/Caddyfile`:

```caddy
api.your-domain.com {
    reverse_proxy 127.0.0.1:8080
}
```

```bash
sudo systemctl reload caddy
```

Caddy provisions and renews the certificate automatically. Optionally restrict
to the API surface and add an extra guard at the edge:

```caddy
api.your-domain.com {
    @api path /api/*
    handle @api {
        reverse_proxy 127.0.0.1:8080
    }
    respond 404
}
```

## 3. Discord OAuth (dashboard login)

The dashboard authenticates users with Discord so it knows which guilds they
manage and what `X-Actor-Id` to forward.

In the [Discord Developer Portal](https://discord.com/developers/applications) →
your application → **OAuth2**:

- **Redirect URIs**: add the dashboard callback(s), e.g.
  - `http://localhost:3000/api/auth/callback/discord` (local dev)
  - `https://dashboard.your-domain.com/api/auth/callback/discord` (prod)
- **Scopes**: `identify guilds`
  - `identify` → the user's id (this becomes `X-Actor-Id`).
  - `guilds` → the user's guild list, so the dashboard can intersect it with
    `GET /api/v1/guilds` and show only guilds where both the user and the bot
    are present (and the user has Manage Server).
- Copy the **Client ID** and **Client Secret** for the env vars below.

The bot already requires the privileged `GUILD_MEMBERS` and `MESSAGE_CONTENT`
intents (see the project README) — unrelated to OAuth, but enable them on the
same application.

## 4. Vercel environment variables (dashboard)

Set these on the dashboard project (Auth.js / NextAuth naming shown):

| Variable             | Purpose                                                            |
| -------------------- | ----------------------------------------------------------------- |
| `AUTH_DISCORD_ID`    | Discord OAuth Client ID                                            |
| `AUTH_DISCORD_SECRET`| Discord OAuth Client Secret                                        |
| `AUTH_SECRET`        | Auth.js session secret (`openssl rand -hex 32`)                   |
| `BENNY_API_BASE_URL` | Base URL of this API, e.g. `https://api.your-domain.com/api/v1`    |
| `BENNY_API_TOKEN`    | The bot's `dashboard_api_token` (server-side only — never exposed) |

`BENNY_API_TOKEN` must only ever be read by server-side code (route handlers /
server actions). Do not prefix it with `NEXT_PUBLIC_`.

Example BFF fetch (server-side):

```ts
const res = await fetch(`${process.env.BENNY_API_BASE_URL}/guilds/${gid}/prefixes`, {
  method: "PUT",
  headers: {
    "Authorization": `Bearer ${process.env.BENNY_API_TOKEN}`,
    "X-Actor-Id": session.user.discordId, // from the Discord OAuth session
    "Content-Type": "application/json",
  },
  body: JSON.stringify({ prefixes: ["!", "?"] }),
});
```

## 5. Local development

1. Put a `dashboard_api_token` in the bot's `config.json` (any value for dev).
2. Run the bot — the API comes up on `http://127.0.0.1:8080`.
3. Point the dashboard at it: `BENNY_API_BASE_URL=http://127.0.0.1:8080/api/v1`,
   `BENNY_API_TOKEN=<the dev token>`.
4. Smoke test from a shell (replace the ids):

   ```bash
   TOKEN=<dev token>
   curl -s http://127.0.0.1:8080/api/v1/guilds \
     -H "Authorization: Bearer $TOKEN" -H "X-Actor-Id: 123456789012345678"

   curl -s -X PUT http://127.0.0.1:8080/api/v1/guilds/<gid>/prefixes \
     -H "Authorization: Bearer $TOKEN" -H "X-Actor-Id: 123456789012345678" \
     -H "Content-Type: application/json" -d '{"prefixes":["!","?"]}'
   ```

   A guild the bot is not in returns `404`. A missing/incorrect token returns
   `401`. A blacklisted `X-Actor-Id` returns `403`.

## Conventions and limits

- **All Discord ids are strings** on the wire (request and response) — JSON
  numbers lose precision above 2^53.
- `PUT` replaces the entire resource (e.g. `prefixes`, `autoroles` are full-list
  replacements). `PATCH` (tags only) is a partial update.
- Reuse the bot's limits: ≤15 prefixes (≤25 chars each), ≤25 autoroles, tag name
  ≤32 chars (`[A-Za-z0-9_-]`, not a reserved word), tag content ≤2000 chars,
  sentinel thresholds in `[0.0, 1.0]`, ≤50 level rewards, automod
  `timeout_secs` in `[60, 2419200]`.
- Levels, starboard and automod each keep a module-private `CONFIG_CACHE` inside
  their cog. Their handlers write through the cog's own `update_config` rather
  than touching SeaORM directly, so the row and that cache can't diverge.
- `/music` is the one resource with no database behind it: it reads the guild's
  live Lavalink player. `connected: false` means the bot isn't in a voice
  channel here; `available: false` means there is no Lavalink node at all.
  Neither is an error, so both return `200`.

## Out of scope for v1 (planned)

Per-user resources are **not** exposed yet and are planned for a later revision:

- Reminders (`reminders_reminders`).
- User settings (`settings_users`: timezone, patron level, blacklist).

The blacklist is *consulted* (a blacklisted `X-Actor-Id` is refused) but is not
*manageable* through v1.
