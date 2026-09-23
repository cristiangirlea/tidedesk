# Running a rendezvous service

The rendezvous service lets a viewer reach a host by its **device ID**
(`TD-1A2B-3C4D-5E6F-7A8B`) instead of both people typing addresses. It remembers each
registered host's internet address, introduces a viewer and a host to each other, and
then drops out: the two computers punch a direct path and the session runs between
them. **It never carries session data.** See the [design notes](design/nat-traversal.md).

Status: the service is ready to run; hosts and viewers start using it in a later
release.

## What it needs

- Any small Linux or Windows machine with a public IPv4 address.
- Two UDP ports open to the internet: `47900` and `47901` (the second lets hosts detect
  a symmetric NAT). No TCP, no TLS certificate, no database: all state is in memory.
- Memory: a few hundred bytes per registered host; 10,000 hosts fit in a few MB.

## Build and run

```
cargo build --locked --release -p tidedesk-rendezvous
./target/release/tidedesk-rendezvous --listen 0.0.0.0:47900 --alt-listen 0.0.0.0:47901
```

`--ttl` (seconds, default 75) sets how long a registration lasts without a refresh.
Hosts refresh every 25 seconds.

With Docker, from the repository root:

```
docker build -f deploy/Dockerfile -t tidedesk-rendezvous .
docker run -d --restart=always -p 47900-47901:47900-47901/udp tidedesk-rendezvous
```

With systemd, use [`deploy/tidedesk-rendezvous.service`](../deploy/tidedesk-rendezvous.service):
it runs the service as an unprivileged dynamic user with a read-only system.

Allow UDP `47900-47901` inbound in the machine's firewall and any cloud security group.

## Why there is no TLS

Nothing secret passes through the service. A host proves it owns its device ID by
signing the service's fresh challenge with its certificate's key, and the ID is the
start of that certificate's SHA-256, so no one else can register it. The session itself
is QUIC with TLS 1.3 directly between the two computers, and the viewer checks the
host's certificate there. A service that lied could at most send a viewer to the wrong
address, where the fingerprint check fails.

## What the operator can and cannot see

Can see, while a host is registered or a lookup happens:

- the device IDs of registered hosts and the public IP address and port each registered
  from;
- which public address looked up which ID, and when.

Cannot see: access codes, screens, audio, input, clipboard or any other session data,
host names, or anything once the two computers are connected.

The service keeps this in memory only and logs counts every five minutes, never IDs or
addresses. Operators publishing a service for others should still say so in their own
privacy notice.

## Limits and abuse

- Each IP address may send 10 datagrams per second (bursts of 20); the rest are dropped.
  At most 100,000 addresses are tracked at once; during a flood from more, new
  addresses wait.
- A Hello is padded to be at least as large as its answer. Registrations and lookups
  must return a challenge the service sent to the sender's own address, and refreshes
  a secret token, so a forged source address gains nothing: the service introduces
  only addresses that proved they are real.
- Registrations expire 75 seconds after the last refresh.
- At most 10,000 hosts are registered at once, and at most 32 from one IP address;
  beyond that new hosts are refused.
- Anyone who knows a device ID can learn that host's public address while it is
  registered, much as with a dynamic-DNS name. The access code, its throttling and
  fingerprint pinning protect the host itself.
