use std::sync::Arc;
use std::{collections::HashMap, io, net::SocketAddr};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc::Receiver;
use tokio::{io::split, net::TcpStream, sync::mpsc::channel};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::mpsc::Sender,
};
use tokio_rustls::{client::TlsStream, TlsConnector};

use webparse::{BinaryMut, Buf};

use crate::Helper;
use crate::prot::{ProtClose, TransStream};
use crate::{prot::ProtFrame, ProxyResult};

// pub struct Builder {
//     server_addr: SocketAddr,
//     tls_client: Option<Arc<rustls::ClientConfig>>,
//     domain: Option<String>,
// }

// impl Builder {
//     pub fn new(
//         server_addr: SocketAddr,
//         tls_client: Option<Arc<rustls::ClientConfig>>,
//         domain: Option<String>,
//     ) -> Builder {
//         Self {
//             tls_client,
//             domain,
//             server_addr,
//         }
//     }

//     pub async fn connect_tls(self) -> ProxyResult<Server<tokio_rustls::client::TlsStream<TcpStream>>> {
//         let connector = TlsConnector::from(self.tls_client.clone().unwrap());
//         let stream = TcpStream::connect(&self.server_addr).await?;
//         // 这里的域名只为认证设置
//         let domain =
//             rustls::ServerName::try_from(&*self.domain.clone().unwrap_or("soft.wm-proxy.com".to_string()))
//                 .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid dnsname"))?;

//         let stream = connector.connect(domain, stream).await?;
//         Ok(Server::new(stream, self.server_addr, self.tls_client, self.domain))
//     }

//     pub async fn connect(self) -> ProxyResult<Server<TcpStream>> {
//         let stream = TcpStream::connect(self.server_addr).await?;
//         Ok(Server::new(stream, self.server_addr, None, None))
//     }
// }

pub struct CenterClient {
    tls_client: Option<Arc<rustls::ClientConfig>>,
    domain: Option<String>,
    server_addr: SocketAddr,
    stream: Option<TcpStream>,
    tls_stream: Option<TlsStream<TcpStream>>,
    next_id: u32,

    sender_work: Option<Sender<(u32, Sender<ProtFrame>)>>,
    sender: Option<Sender<ProtFrame>>,
    receiver: Option<Receiver<ProtFrame>>,
}

impl CenterClient {
    pub fn new(
        server_addr: SocketAddr,
        tls_client: Option<Arc<rustls::ClientConfig>>,
        domain: Option<String>,
    ) -> Self {
        Self {
            tls_client,
            domain,
            server_addr,
            stream: None,
            tls_stream: None,
            next_id: 1,

            sender_work: None,
            sender: None,
            receiver: None,
        }
    }

    pub async fn transfer<T>()
    where
        T: AsyncRead + AsyncWrite + Unpin,
    {
    }

    async fn inner_connect(
        tls_client: Option<Arc<rustls::ClientConfig>>,
        server_addr: SocketAddr,
        domain: Option<String>,
    ) -> ProxyResult<(Option<TcpStream>, Option<TlsStream<TcpStream>>)> {
        if tls_client.is_some() {
            println!("connect by tls");
            let connector = TlsConnector::from(tls_client.unwrap());
            let stream = TcpStream::connect(&server_addr).await?;
            // 这里的域名只为认证设置
            let domain =
                rustls::ServerName::try_from(&*domain.unwrap_or("soft.wm-proxy.com".to_string()))
                    .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid dnsname"))?;

            let outbound = connector.connect(domain, stream).await?;
            Ok((None, Some(outbound)))
        } else {
            let outbound = TcpStream::connect(server_addr).await?;
            Ok((Some(outbound), None))
        }
    }

    pub async fn connect(&mut self) -> ProxyResult<bool> {
        let (stream, tls_stream) = Self::inner_connect(
            self.tls_client.clone(),
            self.server_addr,
            self.domain.clone(),
        )
        .await?;
        self.stream = stream;
        self.tls_stream = tls_stream;
        Ok(self.stream.is_some() || self.tls_stream.is_some())
    }

    

    pub async fn inner_serve<T>(
        stream: T,
        receiver_work: &mut Receiver<(u32, Sender<ProtFrame>)>,
        receiver: &mut Receiver<ProtFrame>,
    ) -> ProxyResult<()>
    where
        T: AsyncRead + AsyncWrite + Unpin,
    {
        let mut map = HashMap::<u32, Sender<ProtFrame>>::new();
        let mut read_buf = BinaryMut::new();
        let mut write_buf = BinaryMut::new();
        let (mut reader, mut writer) = split(stream);
        let mut vec = vec![0u8; 4096];
        let is_closed;
        loop {
            let _ = tokio::select! {
                r = receiver_work.recv() => {
                    println!("center_client receiver = {:?}", r);
                    if let Some((sock, sender)) = r {
                        map.insert(sock, sender);
                        println!("write create socket");
                        let _ = ProtFrame::new_create(sock, None).encode(&mut write_buf);
                    }
                }
                r = receiver.recv() => {
                    println!("center_client xxxx = {:?}", r);
                    if let Some(p) = r {
                        let _ = p.encode(&mut write_buf);
                    }
                }
                r = reader.read(&mut vec) => {
                    println!("center_client rrrrr = {:?}", r);
                    match r {
                        Ok(0)=>{
                            is_closed=true;
                            break;
                        }
                        Ok(n) => {
                            read_buf.put_slice(&vec[..n]);
                        }
                        Err(_err) => {
                            println!("error === {:?}", _err);
                            is_closed = true;
                            break;
                        },
                    }
                }
                r = writer.write(write_buf.chunk()), if write_buf.has_remaining() => {
                    println!("center_client wwwww = {:?} len = {:?} ", r, write_buf.has_remaining());
                    match r {
                        Ok(n) => {
                            write_buf.advance(n);
                            if !write_buf.has_remaining() {
                                write_buf.clear();
                            }
                        }
                        Err(e) => {
                            println!("center_client errrrr = {:?}", e);
                        },
                    }
                }
            };

            loop {
                match Helper::decode_frame(&mut read_buf)? {
                    Some(p) => {
                        println!("server receiver = {:?}", p);
                        if p.is_create() {
                            // let (virtual_sender, virtual_receiver) = channel::<ProtFrame>(10);
                            // map.insert(p.sock_map(), virtual_sender);
                            // let stream =
                            //     VirtualStream::new(p.sock_map(), sender.clone(), virtual_receiver);

                            // let (flag, username, password, udp_bind) = (option.flag, option.username.clone(), option.password.clone(), option.udp_bind.clone());
                            // tokio::spawn(async move {
                            //     let _ = Proxy::deal_proxy(stream, flag, username, password, udp_bind).await;
                            // });
                        } else if p.is_close() {
                            if let Some(sender) = map.get(&p.sock_map()) {
                                let _ = sender.try_send(p);
                            }
                        } else if p.is_data() {
                            if let Some(sender) = map.get(&p.sock_map()) {
                                let _ = sender.try_send(p);
                            }
                        }
                    }
                    None => {
                        break;
                    }
                }

                if !read_buf.has_remaining() {
                    read_buf.clear();
                }
            }
        }
        if is_closed {
            for v in map {
                let _ = v.1.try_send(ProtFrame::Close(ProtClose::new(v.0)));
            }
        }
        Ok(())
    }

    pub async fn serve(&mut self) {
        let tls_client = self.tls_client.clone();
        let server = self.server_addr.clone();
        let domain = self.domain.clone();

        let (sender_work, mut receiver_work) = channel::<(u32, Sender<ProtFrame>)>(10);
        let (client_sender, mut client_receiver) = channel::<ProtFrame>(10);
        let stream = self.stream.take();
        let tls_stream = self.tls_stream.take();
        self.sender_work = Some(sender_work);
        self.sender = Some(client_sender);

        tokio::spawn(async move {
            let mut stream = stream;
            let mut tls_stream = tls_stream;
            loop {
                if stream.is_some() {
                    let _ = Self::inner_serve(stream.take().unwrap(), &mut receiver_work, &mut client_receiver).await;
                } else if tls_stream.is_some() {
                    let _ = Self::inner_serve(tls_stream.take().unwrap(), &mut receiver_work, &mut client_receiver).await;
                };
                match Self::inner_connect(tls_client.clone(), server.clone(), domain.clone()).await {
                    Ok((s, tls)) => {
                        stream = s;
                        tls_stream = tls;
                    }
                    Err(_err) => {
                    },
                }
            }
        });
    }

    fn calc_next_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 2;
        id
    }

    pub async fn deal_new_stream<T>(&mut self, inbound: T) -> ProxyResult<()>
    where
        T: AsyncRead + AsyncWrite + Unpin + Send + Sync + 'static,
    {
        let id = self.calc_next_id();
        let sender = self.sender.clone();
        let (stream_sender, stream_receiver) = channel::<ProtFrame>(10);
        let _ = self.sender_work.as_mut().unwrap().send((id, stream_sender)).await;
        tokio::spawn(async move {
            let trans = TransStream::new(inbound, id, sender, Some(stream_receiver));
            let _ = trans.copy_wait().await;
        });
        Ok(())
    }
}
