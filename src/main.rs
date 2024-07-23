use bytes::Buf;
use std::collections::VecDeque;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    select,
};

#[tokio::main]
async fn main() {
    let mut args: VecDeque<_> = std::env::args().collect();
    args.pop_front();

    if args.is_empty() {
        return usage();
    }

    let mut listen = false;
    if args[0] == "-l" {
        listen = true;
        args.pop_front();
    }

    let Some(tcp_port) = args.pop_front() else {
        return usage();
    };
    let Some(udp_port) = args.pop_front() else {
        return usage();
    };
    let Some(udp_dst) = args.pop_front() else {
        return usage();
    };

    let Ok(tcp_port) = tcp_port.parse::<u16>() else {
        eprintln!("invalid tcp port {tcp_port}");
        return usage();
    };
    let Ok(udp_port) = udp_port.parse::<u16>() else {
        eprintln!("invalid udp port {udp_port}");
        return usage();
    };
    let Ok(udp_dst) = udp_dst.parse::<SocketAddr>() else {
        eprintln!("invalid udp endpoint {udp_dst}");
        return usage();
    };

    let udp =
        tokio::net::UdpSocket::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), udp_port))
            .await
            .expect("udp-bind");
    let mut listener = if listen {
        Some(
            tokio::net::TcpListener::bind(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                tcp_port,
            ))
            .await
            .expect("tcp-listen"),
        )
    } else {
        None
    };
    let mut tcp = if listen {
        None
    } else {
        Some(
            tokio::net::TcpStream::connect(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                tcp_port,
            ))
            .await
            .expect("tcp-connect"),
        )
    };

    let mut udp_buf = Vec::with_capacity(4096);
    let mut tcp_buf = Vec::with_capacity(4096);

    loop {
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
            conn = listener_fut => {
                let (conn, _addr) = conn.expect("tcp-accept");
                eprintln!("replacing tcp connection");
                tcp = Some(conn);
            }
            msg = udp.recv_buf(&mut udp_buf) => {
                if let Some(tcp) = &mut tcp {
                    let _ = msg.expect("udp-recv");
                    let len = udp_buf.len() as u32;
                    tcp
                        .write_all_buf(
                            &mut Buf::chain(&len.to_le_bytes()[..], &udp_buf[..])
                        )
                        .await
                        .expect("tcp-write");
                    udp_buf.clear();
                } else {
                    eprintln!("dropping udp packet without a tcp peer");
                }
            }
            msg = tcp_fut => {
                let n = msg.expect("tcp-read");
                if n == 0 {
                    eprintln!("tcp connection dropped");
                    tcp = None;
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
                    udp.send_to(msg, udp_dst).await.expect("udp-send");
                }

                if rest.is_empty() {
                    tcp_buf.clear();
                } else {
                    let cp = Vec::from(rest);
                    tcp_buf = cp;
                }
            }
        }
    }
}

fn usage() {
    eprintln!("udp-over-tcp [-l] <tcp port> <udp port> <udp dst>");
}
