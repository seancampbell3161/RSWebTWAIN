# Privacy Policy

RSWebTWAIN runs entirely on the end user's machine. It does not collect,
transmit, or store personal data, telemetry, analytics, or crash reports. It
does not contact any remote server. It does not include any auto-update
mechanism at this time.

## Local network behaviour

The agent listens on `127.0.0.1:47115` (IPv4 loopback) for WebSocket
connections from local browser applications. It does not accept connections
from non-loopback addresses. The listening port and the set of accepted browser
origins are configurable; see [configuration.md](configuration.md).

## Local data

Configuration is stored locally under `%APPDATA%\com.rswebtwain.app\`, in
plain text. If a deployer sets an optional auth token (see
[integration.md](integration.md#optional-shared-token)), it is stored in
that same config file, also in plain text.

## Scanned images

Scanned image data flows from the scanner driver, through the agent, to the
requesting browser application over the local WebSocket. RSWebTWAIN does not
retain, copy, or upload scanned images.

## Future changes

If a future version introduces any data collection — for example, opt-in crash
reporting or auto-update version checks — this policy will be updated and the
change announced in the release notes.

## Contact

Questions about this policy: <sean.campbell3161@gmail.com>.
