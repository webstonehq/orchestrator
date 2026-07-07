# Deploy to Railway

Orchestrator ships as a prebuilt multi-arch image
(`ghcr.io/webstonehq/orchestrator:latest`), so a Railway deploy is just that
image plus a persistent volume. This page covers the one-click template, how to
build the template yourself, and the two settings that are easy to miss.

> ⚠️ **Orchestrator has no built-in authentication.** A Railway service with a
> public domain is reachable by anyone who has the URL — and that means full
> control of your flows, runs, and decrypted secrets. Do not put real secrets in
> a publicly exposed instance. Gate it behind Railway's access controls, an
> authenticating proxy, or private networking first. See the README
> [Security notes](../README.md#security-notes).

## One-click deploy

[![Deploy on Railway](https://railway.com/button.svg)](https://railway.com/template/REPLACE_WITH_TEMPLATE_CODE)

> Replace `REPLACE_WITH_TEMPLATE_CODE` with the code from your published template
> URL (see [Publish the template](#publish-the-template) below). Until the
> template is published, use the manual steps.

The template provisions a single `orchestrator` service:

| Setting          | Value                                        | Why |
| ---------------- | -------------------------------------------- | --- |
| Source (image)   | `ghcr.io/webstonehq/orchestrator:latest`     | The published image — no build on Railway. |
| Volume mount     | `/data`                                      | Persists the SQLite DB **and** the encrypted `master.key`. Lose it and existing secrets become undecryptable. |
| `RAILWAY_RUN_UID`| `0`                                          | The image runs as a non-root user; Railway mounts volumes as root, so the service must run as root to write `/data`. |
| Public domain    | auto                                         | The app binds `0.0.0.0:$PORT` and Railway injects `PORT`, so the domain "just works" — no target port to set. |
| Healthcheck path | `/api/health`                               | Returns `{"ok":true}`; Railway waits for it before routing traffic. |
| Restart policy   | On failure                                   | Standard. |

## Manual deploy

The same steps double as the recipe for building the template in the composer.

1. **New Project → Empty Project.**
2. **Add a Service → Docker Image.** Enter the full image URL:
   `ghcr.io/webstonehq/orchestrator:latest`.
   - The GHCR package must be **public** (Package settings → Change visibility →
     Public). Otherwise add registry credentials in the service's source
     settings — see Railway's [Private Docker Images](https://docs.railway.com/templates/private-docker-images).
3. **Add a Volume** to the service and set the **mount path to `/data`**.
4. **Variables → add `RAILWAY_RUN_UID` = `0`.** Without this the service crashes
   on boot with a permission error creating `/data/.orchestrator`.
5. **Settings → Networking → Generate Domain.** Railway detects the port from
   the `PORT` it injects (the app honors it). No manual target port needed.
6. **Settings → Deploy → Healthcheck Path → `/api/health`.**
7. **Deploy**, then open the generated domain. The `http.request` plugin ships
   inside the image, so flows run out of the box.

Optional variables:

- `RUST_LOG` — log verbosity (e.g. `info` or `info,orchestrator=debug`).
- `PORT` — leave unset; Railway manages it. The image defaults to `4400` when
  it is absent.

## Publish the template

1. Get the service configured and deployed once (the steps above).
2. From the project, open the **Template Composer** (project settings → or the
   "Publish as Template" action) and confirm the service, volume, and variables.
3. Publish. Railway gives you a template URL like
   `https://railway.com/template/<code>`.
4. Paste `<code>` into the button link at the top of this file so the badge
   points at your template.

See Railway's [Create a Template](https://docs.railway.com/templates/create)
docs for the current composer flow.

## Persistence & backups

Everything lives under the `/data` volume: `orchestrator.db` (flows, runs,
schedules, secret ciphertext) and `master.key` (the key that decrypts those
secrets). Keep the volume across redeploys, and back it up if the data matters —
a lost `master.key` means unrecoverable secrets.

## Workers (optional, later)

This template is server-only. To run remote execution ([BYOW](../README.md)),
add a second service on the same image with a start command of
`worker --server <internal-url> --token <token> --queues <…>`, pointed at the
server over Railway's private network, and set a shared token on the server via
`--worker-token` / `ORCH_WORKER_TOKENS`.
