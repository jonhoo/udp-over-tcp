# udp-over-tcp

A command-line tool for tunneling UDP datagrams over TCP.

It is particularly useful for [tunneling UDP over SSH][so]

The tool was designed primarily for the use-case where you have two
applications that need to talk to each other over UDP without an obvious
client-server relationship. That is, either application may initiate a
packet, and must be configured with the address of the other at
start-up (i.e., ports cannot be random).

You run `udp-over-tcp` on both applications' hosts. Each instance acts
as a kind of local replica on the application running on the other. If
the application on one host listens on UDP port P, then `udp-over-tcp`
will listen on UDP port P on the _other_ host, and ensure that traffic
to the replica P goes to the real application's port P. Crucially,
`udp-over-tcp` will do this for both applications' ports at the same
time and bond the ports. It will effectively "pretend" that each
application is running locally to the other.

Concretely, if the application on one host sends a datagram from its
port P to (local replica) port Q, the datagram will arrive at the real
(remote) application's port Q *with a source port of P*. This means that
an application always sees the same single address (localhost)
and port (the other application's port), and that same address-host pair
can also be used in the peer configuration for the application.

Hopefully the following diagram can aid in understanding the setup
`udp-over-tcp` configures:

[![Diagram showing the intended network setup of udp-over-tcp when run
across an SSH tunnel.](diagram.svg)][diag]

## Installation

The program [comes pre-compiled][rel] for a number of platforms (thanks
[cargo-dist]!), and should be executable out of the box with no
dependencies.

Alternatively, you can install it through Cargo with

```console
$ cargo install udp-over-tcp
```

[so]: https://superuser.com/questions/53103/udp-traffic-through-ssh-tunnel/
[diag]: https://excalidraw.com/#json=oIUskge-sbnxvosJ5GMiz,9cF_06fOe8FImnZVMQboNQ
[rel]: https://github.com/jonhoo/udp-over-tcp/releases
[cargo-dist]: https://opensource.axo.dev/cargo-dist/

## Usage

You have a UDP application running on host X on port A.
You want it to talk to a UDP application running on host Y on port B.
And you also want to allow the application on Y to talk to A on X.
Great, do as follows:

On either host (here X), first create a TCP tunnel to the other host:

    ssh -L 7878:127.0.0.1:7878 $Y

Next, run udp-over-tcp on both hosts, one with `--tcp-listen` and one with `--tcp-connect`.
The `--tcp-listen` should be used on the host that the forwarding allows connecting _to_ (here Y).
You can run them in either order, but best practice is to listen first:

    Y $ udp-over-tcp --tcp-listen  7878 --udp-bind $A --udp-sendto $B
    X $ udp-over-tcp --tcp-connect 7878 --udp-bind $B --udp-sendto $A

On Y, this will listen on UDP port $A, forward those over TCP to X, and then deliver them to UDP port $A there.
On X, this will listen on UDP port $B, forward those over TCP to Y, and then deliver them to UDP port $B there.

Now configure the application on X to send to 127.0.0.1:$B
and configure the application on Y to send to 127.0.0.1:$A.
In other words, same port, local IP address.

Each argument takes a port number (as above) or addr:port to specify the address.
(address defaults to 0.0.0.0 for listen/bind and 127.0.0.1 for connect/sendto)

## Alternatives

There exist other tools that can help with this problem, though they
have different properties than this tool.

Solutions relying on `nc` or `socat` do not preserve UDP datagram
boundaries, meaning two UDP `sendmsg` can cause only a single (combined)
message to arrive through `recvfrom`. Many UDP applications are not
resilient to this as they rely on UDP to provide message framing.

[mullvad's udp-over-tcp][mullvad] only provides unidrectional
forwarding. One can run additional instances of the tool to forward in
the other direction, though doing so means the source port of incoming
datagrams will not match the destination port of outgoing datagrams.
However, this is likely fine for client-server style applications where
the client's port isn't important.

[mullvad]: https://github.com/mullvad/udp-over-tcp

## License

Licensed under either of

 * Apache License, Version 2.0
   ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
 * MIT license
   ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

## Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
