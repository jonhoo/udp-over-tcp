use bytes::Buf;
use eyre::WrapErr;
use lexopt::prelude::*;
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
    let mut tcp_port = None;
    let mut udp_port = None;
    let mut udp_dst = None;

    let mut parser = lexopt::Parser::from_env();
    while let Some(arg) = parser.next().wrap_err("parse arguments")? {
        match arg {
            Short('l') => {
                listen = true;
            }
            Value(port) if tcp_port.is_none() => {
                tcp_port = Some(port.parse::<u16>().wrap_err("invalid tcp port")?);
            }
            Value(port) if udp_port.is_none() => {
                udp_port = Some(port.parse::<u16>().wrap_err("invalid udp port")?);
            }
            Value(dst) if udp_dst.is_none() => {
                udp_dst = Some(
                    dst.parse::<SocketAddr>()
                        .wrap_err("invalid forward destination")?,
                );
            }
            Short('h') | Long("help") => {
                usage();
            }
            _ => return Err(arg.unexpected()).wrap_err("unexpected argument"),
        }
    }

    let Some(tcp_port) = tcp_port else {
        eyre::bail!("no tcp port given");
    };
    let Some(udp_port) = udp_port else {
        eyre::bail!("no udp port given");
    };
    let Some(udp_dst) = udp_dst else {
        eyre::bail!("no udp forward destination given");
    };

    let udp_src = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), udp_port);
    tracing::debug!("bind to udp {udp_src:?}");
    let udp = tokio::net::UdpSocket::bind(udp_src)
        .await
        .expect("udp-bind");
    let mut listener = if listen {
        let tcp_src = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), tcp_port);
        tracing::info!("bind to tcp {tcp_src:?}");
        Some(
            tokio::net::TcpListener::bind(tcp_src)
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

                let tcp_dst = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), tcp_port);
                tracing::debug!("connect to tcp {tcp_dst:?}");
                tokio::net::TcpStream::connect(tcp_dst).await
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
                    if let Err(e) = udp.send_to(msg, udp_dst).await {
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
    eprintln!("udp-over-tcp [-l] <tcp port> <udp port> <udp dst>");
    std::process::exit(0);
}
