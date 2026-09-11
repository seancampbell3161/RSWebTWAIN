# Integration Guide

How a browser application connects to the RSWebTWAIN agent, what the
WebSocket protocol looks like on the wire, and what security guarantees the
connection does and does not provide.

## Prerequisites

- The agent is installed and running. It is a headless system tray
  application — there is no window to open. A tray icon is the only visible
  sign it is running.
- It runs as a background service for the current Windows user; it does not
  start a browser or a UI of its own. Your page connects to it, not the
  other way around.

## Connecting

Open a WebSocket to:

```
ws://127.0.0.1:47115
```

47115 is the default port. It's configurable, either in `config.toml`
(`[server]` `port = ...`) or via the `RSWEBTWAIN_PORT` environment variable.
There is no discovery mechanism — if you're targeting an installation that
has been reconfigured to a non-default port, your page needs to be told
that port some other way (a settings screen, a query parameter, etc.).

If your page is served over `https://`, see [Mixed content](#mixed-content)
below before you wire this up.

A `ping` is the simplest way to confirm the connection is alive before doing
anything else:

```jsonc
// → agent
{"type":"ping","id":"req-0"}
// ← agent
{"type":"pong","id":"req-0"}
```

## Origins

With no config file, the agent accepts connections whose `Origin` header is
`http://` or `https://` `localhost`, `127.0.0.1`, or `[::1]`, on any port,
and rejects everything else. That covers local development and most
single-machine deployments without any configuration.

A production frontend served from a real domain must be added explicitly.
Edit `config.toml`:

```toml
[server]
extra_origins = ["https://app.example.com"]
```

`allow_localhost` stays `true` by default even after you add `extra_origins`
— adding a production origin does not, by itself, stop local pages from
connecting too. Set `allow_localhost = false` if you want to block
localhost origins entirely.

## Mixed content

The agent serves plain `ws://` on loopback only — it has no TLS support and
ships no certificate. A page loaded over `https://` that opens a `ws://`
connection is making a mixed-content request, and browsers restrict mixed
content. How far that restriction goes — whether it's blocked outright or
loopback addresses are treated as an exception — has varied across browsers
and versions, so this guide won't claim a fixed answer here; verify it in
the browsers your integration needs to support.

In practice: a page served over plain `http://`, including
`http://localhost` during development, is unaffected. A production page
served over `https://` is the case at risk, and when a browser does block
the connection it typically doesn't surface a clear "mixed content" error —
the WebSocket just fails to open.

There's no server-side fix available — the agent doesn't offer a `wss://`
option. If mixed-content restrictions affect the browsers you target, treat
it as a constraint to design around rather than something the agent can be
configured to avoid.

## A worked exchange

Every request carries a correlation `id`, and every response to it echoes
that same `id` back — including each `scan_progress` / `binary_start`
event during a scan, which all echo the `id` of the `start_scan` that
started them (not a new one per event). The binary frames that follow a
`binary_start` carry no `id` of their own — see
[Receiving binary transfers](#receiving-binary-transfers) below.

```jsonc
// → agent
{"type":"list_scanners","id":"req-1"}
// ← agent
{"type":"scanner_list","id":"req-1",
 "scanners":[{"id":"1","name":"HP ScanJet","manufacturer":"HP"}]}

// → agent. Every option below is optional; defaults shown.
{"type":"start_scan","id":"req-2","options":{
  "scanner_id":"HP ScanJet",   // null or omitted = default scanner
  "resolution":300,
  "color_mode":"color",        // "color" | "grayscale" | "bw"
  "duplex":false,
  "use_adf":false,
  "format":"pdf",              // "pdf" | "png" | "jpeg"
  "show_scanner_ui":false
}}

// ← agent, per page
{"type":"scan_progress","id":"req-2","scan_id":"a1b2","page":1,"status":"scanning"}
{"type":"binary_start","id":"req-2","scan_id":"a1b2","kind":"thumbnail",
 "page":1,"mime":"image/jpeg","total_bytes":20481}
// → then binary WebSocket frames totalling 20481 bytes

// ← agent, once the batch is done (PDF output only)
{"type":"scan_progress","id":"req-2","scan_id":"a1b2","page":60,"status":"processing"}
{"type":"binary_start","id":"req-2","scan_id":"a1b2","kind":"pdf",
 "mime":"application/pdf","total_bytes":4194304}
// → then binary frames totalling 4194304 bytes

{"type":"scan_complete","id":"req-2","scan_id":"a1b2","total_pages":60}

// → agent, to stop an in-flight scan
{"type":"cancel_scan","id":"req-3","scan_id":"a1b2"}

// ← agent, on any failure
{"type":"error","id":"req-2","code":"SCANNER_BUSY","message":"A scan is already in progress"}
```

`scanner_id` in `start_scan` matches either the `id` or the `name` a prior
`scanner_list` returned for that scanner — despite the field name, passing
the human-readable name (as shown above) works.

Three behaviours worth knowing before you write a client against this:

- **A `"pdf"` format streams thumbnails, not full pages.** Every per-page
  transfer during a PDF scan is `kind: "thumbnail"` — a JPEG downscaled so
  its long edge is at most 300px, intended for a progress display, not for
  viewing the page at full quality. The full-resolution pages live only
  inside the finished PDF, which arrives as a single `kind: "pdf"` transfer
  once all pages are in. In `"png"` and `"jpeg"` output, there is no
  thumbnail: `kind: "page"` carries the full-resolution image directly. If
  PDF assembly fails, the scan does not complete — the agent sends `error`
  with code `PDF_GENERATION_ERROR` instead of `scan_complete`.
- **Only one scan runs at a time.** A `start_scan` sent while another scan
  is in progress is rejected immediately with `error` / `SCANNER_BUSY`; it
  does not queue.
- **`scan_id` is assigned by the agent, not the client**, and only appears
  once the first `scan_progress` for that scan arrives. To cancel a scan,
  capture `scan_id` from that first message. Also note that the agent's
  acknowledgment of a `cancel_scan` is a `scan_progress` message carrying
  the `cancel_scan` request's own `id` (not the original `start_scan`'s
  `id`) — it confirms the cancellation was received, not that the scan has
  actually stopped. The scan's own `id` later receives either a normal
  `scan_complete` (if it finished before the cancellation took effect) or
  an `error` with code `SCAN_CANCELLED`.

## Receiving binary transfers

Page images and the finished PDF are not inlined in JSON. Each one is
announced by a `binary_start` message, and the bytes follow immediately
after as one or more binary WebSocket frames, totalling `total_bytes`
bytes.

- **Accumulate frames until you've received `total_bytes` bytes**, then
  treat the transfer as complete. Do not assume a frame count — the chunk
  size the agent splits a payload into is an implementation detail and may
  change without notice.
- **Text frames can interleave with a transfer in progress** — a `pong`
  answering an unrelated `ping`, or the next `scan_progress`, may arrive
  between binary frames. The WebSocket frame type (text vs. binary) is what
  tells them apart, so handle the two independently rather than assuming
  `binary_start` and its frames strictly alternate with nothing else
  between them.
- **`kind` tells you what the bytes are:** `"thumbnail"` is a downscaled
  preview of one page, sent only for PDF output; `"page"` is the
  full-resolution image of one page, sent only for PNG and JPEG output; and
  `"pdf"` is the finished, assembled document, sent once, after the last
  page.

This accumulation rule is the one part of the protocol a client can't
implement from the message list alone. A worked example:

```js
socket.binaryType = "arraybuffer";

let pending = null;

socket.onmessage = (event) => {
  if (typeof event.data === "string") {
    const msg = JSON.parse(event.data);
    if (msg.type === "binary_start") {
      pending = {
        kind: msg.kind,
        page: msg.page,
        mime: msg.mime,
        total: msg.total_bytes,
        received: 0,
        chunks: [],
      };
    } else {
      handleMessage(msg); // scan_progress, scan_complete, error, pong
    }
    return;
  }

  // A binary frame belongs to the transfer announced most recently.
  const chunk = new Uint8Array(event.data);
  pending.chunks.push(chunk);
  pending.received += chunk.byteLength;

  if (pending.received >= pending.total) {
    const blob = new Blob(pending.chunks, { type: pending.mime });
    onTransfer(pending.kind, pending.page, blob); // "thumbnail" | "page" | "pdf"
    pending = null;
  }
};
```

The loop above is driven entirely by `total_bytes` — never by a frame count
or an assumed chunk size, both of which may change without notice.

## Error codes

| Code | Cause |
|---|---|
| `SCANNER_NOT_FOUND` | `start_scan` named a `scanner_id` that doesn't match any scanner currently reported by `list_scanners`. |
| `SCANNER_BUSY` | A `start_scan` arrived while another scan was already in progress. |
| `SCAN_CANCELLED` | The scan was stopped by a `cancel_scan` request before it finished. |
| `PAPER_JAM` | The scanner driver reported a paper jam. |
| `PAPER_DOUBLE_FEED` | The scanner driver's multi-feed detector fired. |
| `TWAIN_NOT_INSTALLED` | The TWAIN Data Source Manager (`TWAINDSM.dll`) could not be loaded — no TWAIN runtime is present on the machine. |
| `NO_SCANNERS_AVAILABLE` | `start_scan` didn't name a `scanner_id` and no scanners were found to use as the default. |
| `INTERNAL_ERROR` | An unclassified failure — a sidecar communication error, a panicked scan task, or a TWAIN error the agent doesn't map to a more specific code. |
| `INVALID_REQUEST` | The incoming message wasn't valid JSON for the protocol, or `cancel_scan` named a `scan_id` with no matching active scan. |
| `CAPABILITY_NOT_SUPPORTED` | The scanner driver rejected a requested capability (for example, a resolution or color mode it doesn't support). |
| `DISCOVERY_TIMEOUT` | `list_scanners` took longer than 15 seconds and was abandoned. |
| `IMAGE_CONVERSION_ERROR` | Converting a scanned page to PNG or JPEG failed. |
| `PDF_GENERATION_ERROR` | Assembling scanned pages into a PDF failed. |

## Optional shared token

No token is generated or required by default. Origin validation (above) is
the gate that matters for a normal install.

A deployer who wants a second factor — for example, to separate multiple
users on a shared machine — can set one in `config.toml`:

```toml
[server]
auth_token = "change-me"
```

or via the `RSWEBTWAIN_AUTH_TOKEN` environment variable, which overrides the
config value. Once set, every connection must include it as a query
parameter:

```
ws://127.0.0.1:47115/?token=change-me
```

Stick to URL-safe characters when choosing a token (letters, digits, `-`,
`_`). The agent reads the query string by first splitting on `&` to find
the `token=...` pair, then splitting that pair on the first `=` — so an
embedded `=` in the token's value is preserved correctly and does not need
escaping. Two characters do need it: an unencoded `&` truncates the token
at that point (the parser reads it as the start of the next query
parameter), and an unencoded `%` can be misread as the start of a
percent-escape if followed by hex digits. Percent-encode a token containing
either, or avoid them by choosing a URL-safe token in the first place.

The same value has to go in both places — the config file and the
connecting page. This only makes sense when the deployer controls both; a
token doesn't help a page that has no way to learn the value.

## Security model

- **Origin validation is the primary defence.** A browser sets the `Origin`
  header itself and page JavaScript cannot forge it, so a hostile web page
  cannot pass this check just by asking to.
- The agent listens on loopback (`127.0.0.1`) only. It is not reachable
  from the network.
- **It does not defend against code running as the same Windows user.**
  Anything running as that user can set any `Origin` it wants and can read
  anything the browser itself can read. The optional token does not change
  this — it helps separate different users on a shared machine, and nothing
  more.
- By default, any page served from `localhost` can enumerate scanners and
  scan through the agent. Deployers who don't want that should set
  `allow_localhost = false` and list their own origin in `extra_origins`.
