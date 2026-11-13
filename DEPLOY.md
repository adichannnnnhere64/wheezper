# wheezper on hermes

Deployed stack (docker compose project `wheezper`):

| piece | where |
|---|---|
| `wheezper-server` | container, published on `127.0.0.1:50151` only |
| nginx front door | host, **https://wheezper.pumpkinforge.de** (TLS + per-IP limits) |
| PostgreSQL | container `wheezper-db-1`, volume `wheezper-db` |
| assistant model | container `wheezper-ollama-1`, model `gemma3:1b`, internal only |
| Android APK | https://wheezper-dl.pumpkinforge.de/app-debug.apk |

```
laptop (wheezper CLI, --source dual)  --gRPC/TLS:443-->  nginx --> wheezper-server  --StreamAnswer--> phone
        |                                                   |
   notes/*.md + pending queue                        PostgreSQL + Ollama (local)
```

Only committed transcript lines are published; raw audio never leaves the laptop.

## Files

```
compose.deploy.yml   the stack (build context = this folder)
.env                 secrets: POSTGRES_PASSWORD, WHEEZPER_PAIRING_CODE   (chmod 600)
certs/               fullchain.pem + privkey.pem copied by the certbot deploy hook
app-debug.apk        the installable Android build (also served over https, see above)
```

`.env` holds the **pairing code** the phone asks for:

```bash
grep WHEEZPER_PAIRING_CODE .env
```

## Everyday commands

```bash
cd /home/ubuntu/migration/wheezper
docker compose -f compose.deploy.yml ps
docker compose -f compose.deploy.yml logs -f server
# after a code change: build on the laptop, ship the image (see below)
docker compose -f compose.deploy.yml restart server
```

The server image also contains `wheezper-admin`:

```bash
docker compose -f compose.deploy.yml exec server wheezper-admin \
  --server http://127.0.0.1:50051 pair --code "$WHEEZPER_PAIRING_CODE" --name cli
```

(from a laptop, point `--server` at `https://wheezper.pumpkinforge.de:50061`.)

## Shipping a code change

**Do not build on this box.** A Rust release build wants ~3.7 GB of builder cache
plus deps; the disk sits around 88-91 % used. Build here on the laptop instead
and ship the finished image (~30 MB compressed):

```bash
# from the repo checkout, on the laptop
scripts/deploy-image.sh hermes /home/ubuntu/migration/wheezper
```

`docker save | ssh | docker load` + `docker compose up -d --no-build server`.
The first build is a full Rust build; later ones recompile only the changed
crate (BuildKit keeps `target/`), so a patch like the assistant prompt ships in
~2 minutes.

If you ever must build on the box, budget the space first:

```bash
sudo docker builder prune -af      # before: frees the old cache
docker compose -f compose.deploy.yml up -d --build server
sudo docker builder prune -af      # after: ~3.7 GB back
```

## Front door: nginx on 443, one public port

```
phone/laptop --h2+TLS 443--> nginx (host, LE cert, limits) --grpcs--> 127.0.0.1:50151 (container)
```

* `/etc/nginx/conf.d/wheezper-grpc.conf`: TLS, ALPN h2, `grpc_pass grpcs://127.0.0.1:50151`,
  `grpc_read_timeout`/`grpc_send_timeout` 12h (a transcript stream stays open for
  the whole meeting), `limit_conn 12` and `limit_req 20r/s burst=40` per IP.
* The container maps `127.0.0.1:50151:50051`, so the Rust server is not reachable
  from the network at all. The old public `:50061` endpoint is gone (ufw rule and
  the `DOCKER-USER` exception were removed with it).
* Reload after edits: `sudo docker exec edge-nginx nginx -t && sudo docker exec edge-nginx nginx -s reload`.
* nginx runs `network_mode: host` (project `edge`, `/home/ubuntu/migration/compose/edge/docker-compose.yml`)
  and mounts `/etc/letsencrypt` read-only for the certificate.

## TLS

* DNS `wheezper.pumpkinforge.de` → `A 139.99.42.241`, **DNS only** (grey cloud). The
  Cloudflare proxy is not used: it answers gRPC with `403` unless the zone's
  *gRPC* network setting is enabled, and this zone has it off.
* Certificate: Let's Encrypt via certbot DNS-01
  (`/etc/letsencrypt/live/wheezper.pumpkinforge.de`, renewed by the host cron).
  nginx reads it directly (mounted read-only).
* `/etc/letsencrypt/renewal-hooks/deploy/wheezper-certs.sh` copies the cert into
  `certs/` (owned by uid 10001, the container user), restarts the server and
  reloads nginx, so a renewal cannot leave a stale cert in either place.

## Firewall / inbound

* `ufw allow 443/tcp` — the only wheezper port open to the world.
* `/usr/local/sbin/docker-user-lockdown.sh` (systemd `docker-user-lockdown`)
  still drops every inbound connection to Docker-published ports from `ens3`;
  wheezper needs no exception any more because nginx is a host process and
  ufw governs it. Re-run after editing: `sudo systemctl restart docker-user-lockdown`.

Nothing else is exposed: db, ollama and the gRPC container publish loopback
ports only.

## Cloudflare (DONE — proxied, gRPC on, origin locked to CF ranges)

State since 2026-11-11: zone **Network → gRPC = On**, `wheezper.pumpkinforge.de`
is a **proxied** A record, UFW allows 443 only from Cloudflare v4/v6 and the
`real_ip` block is live (nginx access log shows the client IP, not an edge IP).
Measured through the edge: pair/create/publish/ask all work (`cfOrigin;dur≈240`
ms) and a `StreamTranscripts` stream survived **100 s of silence** — Cloudflare
does not drop idle gRPC streams at that scale, so meeting pauses are safe.

Everything below is kept for a rebuild or a rollback.

The tunnel cannot carry this: Cloudflare's own docs say public-hostname tunnels
do not support gRPC (only private subnet routing does), and a zone with the
*Network → gRPC* setting off answers `application/grpc` with `403`. So the path
is proxied DNS straight to the origin, not the tunnel:

1. Dashboard → pumpkinforge.de → **Network → gRPC → On**. (Needs
   *Zone Settings:Edit*; neither token on this box has it, so this cannot be
   scripted from here.)
2. Then make `wheezper.pumpkinforge.de` proxied (orange cloud) again — the A
   record already points at `139.99.42.241`.
3. Restrict 443 to Cloudflare's ranges only so the origin is reachable only
   through Cloudflare. **Delete the blanket rule first** — with
   `ufw allow 443/tcp` still present the allow-from rules change nothing:

```bash
sudo ufw delete allow 443/tcp
for ip in $(curl -s https://www.cloudflare.com/ips-v4) $(curl -s https://www.cloudflare.com/ips-v6); do
  sudo ufw allow from "$ip" to any port 443 proto tcp comment 'wheezper via Cloudflare'
done
sudo ufw status numbered | grep 443
```

Verify from a machine that is not Cloudflare: `nc -vz wheezper.pumpkinforge.de 443`
must now time out, while a gRPC call through the name still works.

`wheezper-grpc.conf` already carries the matching `set_real_ip_from` list +
`real_ip_header CF-Connecting-IP`, so the per-IP limits stay per client once the
proxy is in front (refresh the list from `ips-v4`/`ips-v6` and reload nginx when
Cloudflare publishes changes).

Until step 1 is done, the DNS record must stay **DNS only**; proxying it now
would make every gRPC call fail with 403.

After the flip, check the one thing docs do not promise: a transcript stream that
sits idle through a long silence, i.e. leave the phone open on a session with no
speech for >5 minutes. If Cloudflare drops idle streams the app reconnects and
re-syncs by sequence number, but that should be measured once rather than
assumed. Clients already use `https://wheezper.pumpkinforge.de` (443) — there is
nothing left on :50061 to move.

## Assistant

`WHEEZPER_ASSISTANT_URL=http://ollama:11434`, model `gemma3:1b` (2 cores, 3.8 GB
box: a 1B Q4 model is the size that fits, ~8-14 s per answer, first token ~3-4 s).

Answers for a session created with **AI help** use the interview prompt: the
interviewer's line is answered as the candidate in one short sentence, context
limited to the last 12 transcript lines. Sessions without AI help keep the
strict "answer only from the transcript" meeting prompt.

Change the model:

```bash
docker compose -f compose.deploy.yml exec ollama ollama pull qwen2.5:1.5b-instruct
# then set WHEEZPER_ASSISTANT_MODEL in the compose file and restart `server`
# and put the same model name in the phone's Settings → assistant model
```

## Phone

1. Install https://wheezper-dl.pumpkinforge.de/app-debug.apk (Chrome: "install
   unknown apps" for your browser; it is a debug build, so it allows cleartext
   for LAN testing but uses TLS with this server).
2. Settings → server `https://wheezper.pumpkinforge.de` (no port) → pairing code from
   `.env` → **Pair**.
3. **New meeting** → tick **Enable AI help** → copy the command it shows.

## Laptop

```bash
wheezper --source dual \
  --session <SESSION_ID> \
  --server https://wheezper.pumpkinforge.de \
  --session-token <TOKEN>
```

Use `--source dual` for interviews: the mic is `me`, the far side is `them`, and
the phone only auto-answers lines that are not `me`. With `--source mic` the
lines carry no label, so the phone would answer your own speech too.

## Accounts, invites and answer cards

Each person gets a **profile**; a phone is bound to one when it pairs.

```bash
./scripts/admin-profiles.sh profile wife "Mei"          # create a profile
./scripts/admin-profiles.sh summary wife meitext.md     # facts the assistant may use
./scripts/admin-profiles.sh invite  wife 1 7            # one-use code, 7-day expiry
./scripts/admin-profiles.sh devices                     # who is paired to what
./scripts/admin-profiles.sh cards                       # load shared answer cards
```

* The **operator pairing code** (`WHEEZPER_PAIRING_CODE`) still works and puts the
  device on the `default` profile. Invite codes bind to the profile they were
  minted for; only the sha256 is stored, so a database dump hands out nothing.
* A used or unknown code both answer `Unauthenticated`, so codes cannot be probed.
* `profiles.summary` is the **only** source of personal claims in an interview
  answer. No summary → no personal claims, and the model says what it does not have.
* `answer_cards` are matched with Postgres full-text search (OR over the question's
  lexemes). A card above `ts_rank` 0.03 is returned **directly** — curated text
  beats a 1.5B model and cannot hallucinate. Below that, the model answers with the
  card in its prompt as reference.

### Migration numbering (do not repeat this)

The live database has migrations 1–4 applied, but this checkout only contains 1, 2
and 5 ("assistant answer sequence" and "session guidance" were applied from a tree
that is no longer here). `db::migrate` therefore calls `.set_ignore_missing(true)`,
and new migrations must be numbered **above the highest applied version** (5).

## Known limits

* gRPC now goes through the Cloudflare proxy (zone setting `protocols.gRPC` on,
  record proxied, 443 restricted to Cloudflare ranges). A tunnel still cannot
  carry it — see the Cloudflare section above.
* A keepalive ping timeout can drop the first connection on a flaky network; the
  app retries with backoff and re-syncs from PostgreSQL by sequence number.
* One server instance per database (live fan-out is an in-process broadcast).
* The local assistant model is `qwen2.5:1.5b-instruct` — good enough for card-backed
  answers and short technical replies, not for open-ended reasoning. The phone can
  override the model per request (and use Ollama Cloud with its own token).
