# Using TideDesk over the internet

TideDesk never relays a session: every connection runs directly between the two
computers. There are several ways to get there, listed from the simplest.

## 1. Direct connection (experimental, from v0.1.0-alpha.5)

Both computers learn their internet address, and each person types in the other's.
The two computers then open a path through their routers and connect directly. Both
need v0.1.0-alpha.5 or later.

1. **Host:** start TideDesk Host (the **TideDesk** entry in the Start menu for Store
   installs). The Status tab shows its **Internet address**, for
   example `203.0.113.5:40000`. Send it and the access code to the person at the
   viewer.
2. **Viewer:** in the connect window, type the host's internet address and the access
   code, tick **Over the internet**, and press Connect. The window then shows
   **this computer's internet address**; send it to the person at the host.
3. **Host:** type the viewer's address under **Viewer on another network** and press
   **Open**, within two minutes.
4. The viewer connects. On the first connection, check that the fingerprint matches
   the one in the host window.

From a terminal, step 2 is:

```
tidedesk view --internet 203.0.113.5:40000 --code K7QM-3XPA-WZ
```

`--stun a,b` picks other STUN servers. With `--stats` the viewer logs the network
path and its round-trip time, so you can see it goes straight to the host.

**What other servers see.** To learn a computer's internet address, TideDesk asks
public STUN servers (by default Google's and Cloudflare's). They see that computer's
public IP address and port and a 20-byte request, nothing else. The session itself never
passes through them. The host can turn this off or choose other servers under
Settings, Internet.

**When it cannot work.**

- **Symmetric NAT** on either side, common on mobile data and some providers'
  carrier-grade NAT: the router uses a new port for every destination, so no direct
  path can be opened. TideDesk detects this and says so. Use option 2 or 4 instead.
- **Same network:** if both computers have the same internet address, connect to one
  of the host's local addresses (shown in its window) without ticking the box, or
  connect by device ID (below), which tries the host's local addresses itself.
- IPv4 only. Someone must be at the host window to press Open: a host started with
  `--headless` cannot open a path yet. A way to connect by a device ID without that
  step is planned.

**Firewall.** The host sends the first packets towards the viewer, so Windows Firewall
should let the viewer's packets in as answers (not yet confirmed on every network
type). If the connection does not come through, allow TideDesk Host on the network
in use. The viewer needs no inbound rule.

For how it works, see the [design notes](design/nat-traversal.md).

### By device ID (experimental)

A viewer can reach a host by its **device ID** (`TD-1A2B-3C4D-5E6F-7A8B`, shown in
the host window) without anyone typing internet addresses, and a host started with
`--headless` can be reached too. TideDesk's own connection service
(`rendezvous.tidedesk.app`) introduces the two computers; the path and the session are
the same direct ones as above, and the service never carries them. On the same local
network the ID works even without the service or the internet (see below).

1. **Host:** nothing to set up. The Share tab shows the device ID and "Viewers on
   other networks can connect with this ID" once it is registered.
2. **Viewer:** type the device ID where an address goes, and connect. There is no
   fingerprint question: the device ID is the start of the host certificate's
   fingerprint, and a host that does not match it is refused.

From a terminal: `tidedesk view TD-1A2B-3C4D-5E6F-7A8B --code …`. The same limits
apply: IPv4 only, and not between two symmetric NATs.

**Symmetric NATs** (common on mobile data and carrier-grade NAT) give every destination a port
of their own, so the address the connection service saw is not the one the other computer
must use. When punching gets no answer, TideDesk also tries the 16 ports above it for ten
seconds (a few hundred small packets at most), since most such routers hand ports out in turn.
That gets through when one side has such a router and the other does not. It does not when both
do, or when the router picks its ports at random: let the host's router open its port (below),
or use a VPN.

**The router opens the port by itself where it can.** While a host is reachable by device
ID, it asks its router to forward its UDP port: over PCP, the older NAT-PMP (both on the
router's UDP port 5351), or UPnP, which most home routers speak:
- it renews the request every hour and asks the router to close the port when TideDesk quits
  (after a crash or a power cut, the router forgets it within two hours; a UPnP router that
  only keeps mappings until they are removed keeps it until TideDesk next quits normally);
- where the router agrees, viewers whose own network defeats hole punching still get in;
- Settings, Network shows whether the router opened it.

Like port forwarding, this exposes the host directly to the internet (see section 4). Untick
**Ask the router to open TideDesk's port** to stop asking (`port_mapping = false` in
`host.toml`).

To use another service, name it as `rendezvous_server` in the host's `host.toml` (or
`--rendezvous host:port`) and in the viewer's `viewer.toml` (or `--rendezvous`); to
register with none, untick the option in Host Settings (`--no-rendezvous` when headless). A viewer only
contacts the service when it connects by ID.

What a service sees while a host is registered: the host's device ID and the public
address and port it registered from, and which public address looked up which ID.
It also holds the host's local addresses, sealed with the access code so that it
cannot read them, and passes them only to a viewer at the host's own public address:
that viewer opens them with the code and connects directly, since two computers
behind one router rarely reach each other through it. It never sees access codes,
screens, audio, input, clipboard or host names, and nothing once the two computers
are connected. It cannot impersonate a host: the
viewer checks that the host's certificate hashes to the device ID it asked for, and
the access code never passes through the service. A broken or hostile service can at
most send a viewer to a wrong address, where that check fails. See the privacy notes
in the [code signing policy](code-signing-policy.md).

**On the same local network.** While it asks the connection service, the viewer also
asks its own network: a small broadcast query for the ID (24 bytes, UDP port 47800),
which the host with that ID answers. Whichever answers first is used, so a device ID
works at home or in an office with no setup, even when the internet or the service is
down, and the session then takes the direct local path (`--stats` shows "by device ID
on this network"). Only the host with that ID answers, only to computers on its own
network, with a packet the same size as the query; the viewer still checks the
certificate against the ID, so another computer answering in the host's place is
refused. Hosts answer unless **Reachable by device ID on this network** is turned off
under Settings, Network (`lan_discovery = false` in `host.toml`).

Viewers look on UDP port 47800 only: a host set to another port is still found through
the service. Broadcasts stop at routers, so computers on different subnets or VLANs
use the service or the host's address.

## 2. Tailscale

[Tailscale](https://tailscale.com) builds a private encrypted network between your devices and
is free for personal use. It also works where a direct connection cannot, such as on
mobile data.

1. Install Tailscale on both computers and sign in with the same account.
2. On the host, run `tidedesk`.
3. On the viewer, connect to the host's Tailscale name or `100.x.y.z` address, without
   ticking "Over the internet":

   ```
   tidedesk view my-desktop --code K7QM-3XPA-WZ
   ```

Nothing is exposed to the public internet.

## 3. WireGuard

If you run your own [WireGuard](https://www.wireguard.com) VPN (for example on your router),
connect the viewer to the VPN and use the host's VPN address, exactly as above.

## 4. Port forwarding

Routers that speak PCP, NAT-PMP or UPnP do this by themselves while the host is reachable by
device ID (see above). On other routers, or with that turned off on the router:

Forward **UDP** port `47800` on your router to the host computer, then connect to your public
IP address or dynamic-DNS name. With a forwarded port, "Over the internet" also works
without anyone pressing Open, after a few seconds.

This exposes TideDesk directly to the internet. The connection is always encrypted, the access
code is never sent over the network, and repeated wrong codes lock the host out for up to 15
minutes. Even so:

- Keep the default random access code; rotate it with `tidedesk host --new-code`.
- Stop the host when you don't need it.
- Prefer the other options whenever you can.
