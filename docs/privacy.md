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

Configuration and the optional auth token are stored locally under
`%APPDATA%\com.rswebtwain.agent\`. The auth token is encrypted at rest using
Windows DPAPI under the current user's scope.

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
