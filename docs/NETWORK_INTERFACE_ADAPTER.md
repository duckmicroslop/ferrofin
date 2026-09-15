# Network interface adapter decision

Server discovery uses **if-addrs 0.15.0**, with `link-local` enabled, rather than
netdev 0.46.3. Both were the latest non-yanked versions returned by the crates.io
sparse index on 2026-09-14. if-addrs is MIT OR BSD-3-Clause; netdev is MIT.

The comparison used the local Jellyfin checkout (`Jellyfin.Server.csproj` targets
.NET 10) and `src/Jellyfin.Networking/Manager/NetworkManager.cs:GetInterfacesCore`:
include only operationally Up interfaces, preserve each address's prefix and
family-specific interface index, then apply the existing virtual/bind rules.

| Aspect | if-addrs 0.15.0 | netdev 0.46.3, defaults disabled |
| --- | --- | --- |
| Address/prefix/index | Available; Windows uses each address family's index | Available; Windows interface record uses `IfIndex` |
| Error reporting | `io::Result` retains enumeration failure | Public enumeration returns `Vec`; empty does not distinguish failures |
| Linux dependencies | libc | ipnet, libc, mac-addr, netlink-packet-core/route, netlink-sys and transitives |
| Extra metadata | No enrichment | Still reads speeds, DHCP, stats and other metadata |
| Linux state | `IFF_RUNNING`; supplemented with administrative `IFF_UP` | `oper_state` follows sysfs; `Unknown` is not equivalent to .NET Down |
| macOS state | Supplemented with native media-status check | Defaults-off state uses interface flags, also insufficient for exact media-status behavior |

The `link-local` feature is intentional: Jellyfin enumerates those addresses and
lets the resolver choose; silently removing them at the adapter would change policy.
Multicast capability is unused by discovery and existing policy consumers, so this
adapter does not collect it. `IpData.supports_multicast` retains its false default.

## State semantics

Do not filter Linux interfaces using `operstate == "up"`: loopback commonly reports
Unknown but .NET considers its UP/RUNNING flags operational. The .NET 10 native
implementation requires both `IFF_UP` and `IFF_RUNNING`. if-addrs supplies RUNNING;
Linux `/sys/class/net/<name>/flags` supplies administrative UP, read once per distinct
interface. Failure to read flags logs a diagnostic and falls back to RUNNING.
A failed address enumeration logs the original error and uses enabled loopbacks.

On macOS, the adapter mirrors .NET's administrative-UP plus `SIOCGIFMEDIA` valid/active
check. Unsupported media on virtual devices falls back to administrative UP. Other
media-query failures log and exclude the adapter. Windows uses if-addrs' native
`OperStatus` and family-specific indices. Native macOS/Windows execution remains
an acceptance check; cross-compilation does not establish runtime parity.

Relevant runtime source:

- [.NET 10 Linux native interface enumeration](https://github.com/dotnet/runtime/blob/v10.0.0/src/native/libs/System.Native/pal_interfaceaddresses.c)
- [.NET 10 Darwin native interface statistics](https://github.com/dotnet/runtime/blob/v10.0.0/src/native/libs/System.Native/pal_networkstatistics.c)
- [if-addrs 0.15.0 source](https://github.com/messense/if-addrs/tree/v0.15.0)

.NET's Linux implementation also probes ethtool carrier state for certain physical
adapters; the Rust adapter uses kernel RUNNING plus UP without that extra corroboration.
This distinction was not exercised by the available active/down/virtual interfaces.

## Probe evidence

An isolated release-mode executable called each adapter 1,000 times, discarded each
snapshot through `std::hint::black_box`, and divided elapsed wall time by 1,000.
Linux, pinned Rust 1.97.1, 2026-09-14:

| Operation | Mean time per call |
| --- | ---: |
| netdev, default features disabled | 5.479 ms |
| if-addrs with link-local | 274.8 µs |
| if-addrs plus once-per-interface Linux flags reads | 324.1 µs |

This is a local microbenchmark with the host's existing physical, loopback and Docker
interfaces, not a general performance guarantee. Earlier unpinned comparison measured
4.218 ms versus 243.1 µs; the table reports the pinned run. A short synchronous
refresh is supported by these measurements; interface enumeration is not one syscall.

Reproduction dependency declarations:

```toml
[dependencies]
netdev = { version = "0.46.3", default-features = false }
if-addrs = { version = "0.15.0", features = ["link-local"] }
```

Commands used (probe executable was isolated under `/tmp/discovery-adapter-probe`):

```sh
curl -fsSL https://index.crates.io/if/-a/if-addrs
curl -fsSL https://index.crates.io/ne/td/netdev
cargo tree --manifest-path /tmp/discovery-adapter-probe/Cargo.toml --edges normal
cargo run --release --manifest-path /tmp/discovery-adapter-probe/Cargo.toml
ip -j address show
```

The real network probes require netlink access: the sandbox returns EPERM for
if-addrs and an empty vector for netdev. This directly exercised the adapters'
different failure reporting. Native `ip` output and both adapter snapshots agreed
on observed addresses/prefixes; loopback Unknown versus if-addrs Up and netdev's
Down physical/Docker interfaces were reviewed. No adapters were reconfigured or
created, and native Windows/macOS/VPN transition tests were unavailable.

An if-addrs-only isolated probe compiled on pinned Rust for aarch64-apple-darwin and
x86_64-pc-windows-gnu. The complete networking crate also passed `cargo check` for
`aarch64-apple-darwin`, `x86_64-pc-windows-gnu`, and `aarch64-unknown-linux-gnu`
with pinned Rust 1.97.1. Darwin all-targets clippy passed with warnings denied.
These checks include the native Darwin supplement.
