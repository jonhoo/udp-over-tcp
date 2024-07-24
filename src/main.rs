use bytes::Buf;
use eyre::WrapErr;
use lexopt::prelude::*;
use std::ffi::OsString;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    select,
};
use tracing_subscriber;

#[tokio::main(flavor = "current_thread")]
async fn main() -> eyre::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::filter::EnvFilter::from_default_env())
        .init();

    let mut listen = false;
    let mut tcp_addr = None;
    let mut udp_bind = None;
    let mut udp_sendto = None;

    let mut parser = lexopt::Parser::from_env();
    while let Some(arg) = parser.next().wrap_err("parse arguments")? {
        match arg {
            Long("tcp-listen") | Short('l') if tcp_addr.is_none() => {
                listen = true;
                tcp_addr = Some(
                    parser
                        .value()
                        .wrap_err("value missing")
                        .and_then(|v| port_or_addr(v, Ipv4Addr::UNSPECIFIED))
                        .wrap_err("--tcp-listen")?,
                );
            }
            Long("tcp-connect") | Short('t') if tcp_addr.is_none() => {
                listen = false;
                tcp_addr = Some(
                    parser
                        .value()
                        .wrap_err("value missing")
                        .and_then(|v| port_or_addr(v, Ipv4Addr::LOCALHOST))
                        .wrap_err("--tcp-connect")?,
                );
            }
            Long("udp-bind") | Short('u') if udp_bind.is_none() => {
                udp_bind = Some(
                    parser
                        .value()
                        .wrap_err("value missing")
                        .and_then(|v| port_or_addr(v, Ipv4Addr::UNSPECIFIED))
                        .wrap_err("--udp-bind")?,
                );
            }
            Long("udp-sendto") | Short('p') if udp_sendto.is_none() => {
                udp_sendto = Some(
                    parser
                        .value()
                        .wrap_err("value missing")
                        .and_then(|v| port_or_addr(v, Ipv4Addr::LOCALHOST))
                        .wrap_err("--udp-sendto")?,
                );
            }
            Short('h') | Long("help") => {
                usage();
            }
            _ => return Err(arg.unexpected()).wrap_err("unexpected argument"),
        }
    }

    let Some(tcp_addr) = tcp_addr else {
        usage();
    };
    let Some(udp_bind) = udp_bind else {
        eyre::bail!("no udp port given");
    };
    let Some(udp_sendto) = udp_sendto else {
        eyre::bail!("no udp forward destination given");
    };

    tracing::debug!("bind to udp {udp_bind:?}");
    let udp = tokio::net::UdpSocket::bind(udp_bind)
        .await
        .expect("udp-bind");
    let mut listener = if listen {
        tracing::info!("bind to tcp {tcp_addr:?}");
        Some(
            tokio::net::TcpListener::bind(tcp_addr)
                .await
                .expect("tcp-listen"),
        )
    } else {
        None
    };
    let mut tcp = None::<tokio::net::TcpStream>;
    let mut connect_again = None::<Pin<Box<tokio::time::Sleep>>>;

    let mut udp_buf = Vec::with_capacity(65536);
    let mut tcp_buf = Vec::with_capacity(65536);

    loop {
        let has_tcp = tcp.is_some();
        let connect_fut = async {
            if !has_tcp && !listen {
                if let Some(timeout) = &mut connect_again {
                    timeout.await;
                    connect_again = None;
                }

                tracing::debug!("connect to tcp {tcp_addr:?}");
                tokio::net::TcpStream::connect(tcp_addr).await
            } else {
                std::future::pending().await
            }
        };
        let listener_fut = async {
            if let Some(listener) = &mut listener {
                listener.accept().await
            } else {
                std::future::pending().await
            }
        };
        let tcp_fut = async {
            if let Some(tcp) = &mut tcp {
                tcp.read_buf(&mut tcp_buf).await
            } else {
                std::future::pending().await
            }
        };

        select! {
            conn = connect_fut, if !has_tcp && !listen => {
                match conn {
                    Ok(stream) => {
                        tracing::info!("established tcp connection");
                        tcp = Some(stream);
                        tcp_buf.clear();
                    }
                    Err(e) => {
                        tracing::error!("tcp connect failed: {e}");
                        connect_again = Some(Box::pin(tokio::time::sleep(Duration::from_secs(1))));
                    }
                }
            }
            conn = listener_fut, if listen => {
                let (conn, addr) = conn.expect("TcpListener::accept only fails if out of FDs or on protocol errors");
                if let Some(old) = tcp.replace(conn) {
                    tracing::warn!(
                        "new tcp connection from {addr:?} replaces old {:?}",
                        old.peer_addr().expect("TcpStream::peer_addr never fails")
                    );
                } else {
                    tracing::info!("accepted incoming tcp connection from {addr:?}");
                }
                tcp_buf.clear();
            }
            msg = udp.recv_buf(&mut udp_buf) => {
                if let Some(tcp_stream) = &mut tcp {
                    let _ = msg.expect("UdpSocket::recv_from has no relevant error conditions");
                    let len = udp_buf.len() as u32;
                    tracing::trace!(n = len, "forward udp packet to tcp");
                    if let Err(e) = tcp_stream.write_all_buf(&mut Buf::chain(&len.to_le_bytes()[..], &udp_buf[..])).await {
                        tracing::error!("dropping tcp connection after failed write: {e}");
                        tcp = None;
                    } else if let Err(e) = tcp_stream.flush().await {
                        tracing::error!("dropping tcp connection after failed flush: {e}");
                        tcp = None;
                    }
                    udp_buf.clear();
                } else {
                    tracing::debug!("dropping udp packet without a tcp peer");
                }
            }
            msg = tcp_fut => {
                let n = msg.expect("tcp-read");
                if n == 0 {
                    tracing::warn!("dropping disconnected tcp connection");
                    tcp = None;
                    continue;
                }

                let mut rest = &tcp_buf[..];
                loop {
                    if rest.len() < std::mem::size_of::<u32>() {
                        break;
                    }
                    let len = u32::from_le_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
                    let tail = &rest[4..];
                    if tail.len() < len {
                        break;
                    }
                    let msg = &tail[..len];
                    rest = &tail[len..];
                    tracing::trace!(n = len, "forward tcp packet to udp");
                    if let Err(e) = udp.send_to(msg, udp_sendto).await {
                        tracing::error!("udp forward failed: {e}");
                    }
                }

                if rest.is_empty() {
                    tcp_buf.clear();
                } else {
                    tracing::trace!(n = rest.len(), "bytes left over in tcp receive buffer");
                    let keep = tcp_buf.len() - rest.len();
                    tcp_buf.drain(..keep);
                }
            }
        }
    }
}

fn usage() -> ! {
    let bin = std::env::args()
        .next()
        .unwrap_or_else(|| String::from(env!("CARGO_BIN_NAME")));

    eprintln!(
        "{}",
        concat!(env!("CARGO_BIN_NAME"), " ", env!("CARGO_PKG_VERSION"))
    );
    eprintln!("https://github.com/jonhoo/udp-over-tcp");
    eprintln!();
    eprintln!("You have a UDP application running on host X on port A.");
    eprintln!("You want it to talk to a UDP application running on host Y on port B.");
    eprintln!("Great, do as follows:");
    eprintln!();
    eprintln!("On either host (here X), first create a TCP tunnel to the other host:");
    eprintln!();
    eprintln!("    ssh -L 7878:127.0.0.1:7878 $Y");
    eprintln!();
    eprintln!("Next, run udp-over-tcp on both hosts, one with `--tcp-listen` and one with `--tcp-connect`.");
    eprintln!("The `--tcp-listen` should be used on the host that the forwarding allows connecting _to_ (here Y).");
    eprintln!("You can run them in either order, but best practice is to listen first:");
    eprintln!();
    eprintln!("    Y $ {bin} --tcp-listen  7878 --udp-bind $A --udp-sendto $B");
    eprintln!("    X $ {bin} --tcp-connect 7878 --udp-bind $B --udp-sendto $A");
    eprintln!();
    eprintln!("On Y, this will listen on UDP port $A, forward those over TCP to X, and then deliver them to UDP port $A there.");
    eprintln!("On X, this will listen on UDP port $B, forward those over TCP to Y, and then deliver them to UDP port $B there.");
    eprintln!();
    eprintln!("Now configure the application on X to send to 127.0.0.1:$B");
    eprintln!("and configure the application on Y to send to 127.0.0.1:$A.");
    eprintln!("In other words, same port, local IP address.");
    eprintln!();
    eprintln!("Each argument takes a port number (as above) or addr:port to specify the address.");
    eprintln!("(address defaults to 0.0.0.0 for listen/bind and 127.0.0.1 for connect/sendto)");
    std::process::exit(0);
}

fn port_or_addr(arg: OsString, default_addr: Ipv4Addr) -> eyre::Result<SocketAddr> {
    match arg.parse::<SocketAddr>() {
        Ok(addr) => Ok(addr),
        Err(_e) => match arg.parse::<u16>() {
            Ok(port) => Ok(SocketAddr::new(IpAddr::V4(default_addr), port)),
            Err(_e) => {
                eyre::bail!("provided value is not an address or a port number");
            }
        },
    }
}
