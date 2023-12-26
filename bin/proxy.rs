use clap::{value_parser, Arg, Command};
use log::{error, info};

fn main() {
    env_logger::init();

    let matches = Command::new("rvncproxy")
        .about("VNC proxy")
        .arg(
            Arg::new("CONNECT-HOST")
                .help("server hostname or IP")
                .required(true)
                .index(1),
        )
        .arg(
            Arg::new("CONNECT-PORT")
                .value_parser(value_parser!(u16))
                .help("server port (default: 5900)")
                .index(2),
        )
        .arg(
            Arg::new("LISTEN-HOST")
                .help("proxy hostname or IP (default: localhost)")
                .index(3),
        )
        .arg(
            Arg::new("LISTEN-PORT")
                .help("proxy port (default: server port plus one)")
                .index(4),
        )
        .get_matches();

    let connect_host = matches.get_one::<String>("CONNECT-HOST").unwrap();
    let connect_port = matches.get_one::<u16>("CONNECT-PORT").unwrap_or(&5900);
    let listen_host = matches
        .get_one::<String>("LISTEN-HOST").map(|x| x.to_owned())
        .unwrap_or("localhost".to_owned());
    let listen_port = matches
        .get_one::<u16>("LISTEN-PORT").map(|x| x.to_owned())
        .unwrap_or(connect_port + 1);

    info!("listening at {}:{}", listen_host, listen_port);
    let listener =
        match std::net::TcpListener::bind((listen_host.to_owned(), listen_port.to_owned())) {
            Ok(listener) => listener,
            Err(error) => {
                error!(
                    "cannot listen at {}:{}: {}",
                    listen_host, listen_port, error
                );
                std::process::exit(1)
            }
        };

    for incoming_stream in listener.incoming() {
        let client_stream = match incoming_stream {
            Ok(stream) => stream,
            Err(error) => {
                error!("incoming connection failed: {}", error);
                continue;
            }
        };

        info!("connecting to {}:{}", connect_host, connect_port);
        let server_stream = match std::net::TcpStream::connect((
            connect_host.to_owned(),
            connect_port.to_owned(),
        )) {
            Ok(stream) => stream,
            Err(error) => {
                error!(
                    "cannot connect to {}:{}: {}",
                    connect_host, connect_port, error
                );
                client_stream.shutdown(std::net::Shutdown::Both).unwrap();
                continue;
            }
        };

        let proxy = match vnc::Proxy::from_tcp_streams(server_stream, client_stream) {
            Ok(proxy) => proxy,
            Err(error) => {
                error!("handshake failed: {}", error);
                continue;
            }
        };

        match proxy.join() {
            Ok(()) => info!("session ended"),
            Err(error) => error!("session failed: {}", error),
        }
    }
}
