# Herald

## About

Herald is a simple and lightweight service that acts as multiplayer game
lobby / room and proxies messages between peers in same room so that players
can play even when behind NATs, web browsers, etc using TCP and WebSocket
transports.

While the service and message protocol is tailored for Perimeter RTS multiplayer
game, is generic enough to potentially support other room peer topologies,
transports and protocols.

While the native game uses TCP, WebSocket is used for web port. The transport
is handled transparently by Herald to the other peers thus this allows players
to talk each other without doing anything special on the client side.

## HTTP

There is a simple HTTP API to retrieve information about current active rooms.
A token must be set and used to limit the exposure of the API.

The / and /ws can be used for WebSocket connection, the message protocol is same
as with TCP after the Upgrade is done.

## Default ports and their uses

HTTP - 8080/8443 - HTTP port for API and WebSocket, 8443 if a certificate is loaded.
TCP - 11654 - Used by games to interact with Herald.

## Common env's to configure

- RUST_LOG=INFO
- SERVER_HTTP_BIND_ADDRESS = IP:PORT - IP and port to bind the HTTP/HTTPS listener
- SERVER_ALLOWED_GAME_TYPES = perimeter - Type of games allowed to connect
- SERVER_HTTP_TOKEN - Token needed in Authorization Bearer to call APIs
- SERVER_TCP_PUBLIC_ADDRESS = DOMAIN:11654 - TCP Address to show clients so they know where to connect
- SERVER_HTTP_PUBLIC_ADDRESS = https://DOMAIN - HTTP/WebSocket Address to show clients so they know where to connect

(Search config_* functions to see all available envs)
