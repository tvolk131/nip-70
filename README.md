# NIP-70

Note: The functionality here has been integrated into [`nip-55`](https://github.com/tvolk131/nip-55), specifically the `nip_46` module, and this repo is no longer maintained.

The reference implementation of Nostr NIP-70.

NIP-70 defines a protocol for delegating key management and event signing to a dedicated desktop app, similar to what NIP-07 does for web browsers. Think "Alby Chrome extension but for desktop apps".

This crate provides client and server implementations that adhere to the NIP-70 spec. Use the server implementation to create a desktop app which holds a user's Nostr nSec and can sign events on behalf of other desktop apps. Use the client implementation to create a desktop app which needs access to a user's nPub or needs to sign events.
