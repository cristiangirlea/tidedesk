# Using TideDesk over the internet

Built-in relay and NAT traversal are planned for version 0.3 (see the [roadmap](ROADMAP.md)).
The `--relay` option exists but only prints a notice for now.

Until then, pick one of these. They are listed from easiest and safest to least safe.

## 1. Tailscale (recommended)

[Tailscale](https://tailscale.com) builds a private encrypted network between your devices and
is free for personal use.

1. Install Tailscale on both computers and sign in with the same account.
2. On the host, run `tidedesk-host`.
3. On the viewer, connect to the host's Tailscale name or `100.x.y.z` address:

   ```
   tidedesk-view my-desktop --code K7QM-3XPA-WZ
   ```

Nothing is exposed to the public internet.

## 2. WireGuard

If you run your own [WireGuard](https://www.wireguard.com) VPN (for example on your router),
connect the viewer to the VPN and use the host's VPN address, exactly as above.

## 3. Port forwarding

Forward **UDP** port `47800` on your router to the host computer, then connect to your public
IP address or dynamic-DNS name.

This exposes TideDesk directly to the internet. The connection is always encrypted, the access
code is never sent over the network, and repeated wrong codes lock the host out for up to 15
minutes. Even so:

- Keep the default random access code; rotate it with `tidedesk-host --new-code`.
- Stop the host when you don't need it.
- Prefer options 1 or 2 whenever you can.
