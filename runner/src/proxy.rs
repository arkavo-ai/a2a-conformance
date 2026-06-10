use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Raw wire capture for one scenario window.
#[derive(Default, Clone)]
pub struct WireCapture {
    pub client_to_server: Vec<u8>,
    pub server_to_client: Vec<u8>,
}

/// A transparent TCP tap between the client harness and the server harness.
///
/// Bytes are tagged with the scenario selected at the moment they are read.
/// With HTTP keep-alive a connection can straddle scenario windows, so the
/// capture is diagnostic (attached to failing results), never authoritative —
/// checks run on harness-normalized outcomes, not on these bytes.
pub struct CaptureProxy {
    state: Arc<Mutex<ProxyState>>,
}

struct ProxyState {
    target_port: Option<u16>,
    current: String,
    captures: HashMap<String, WireCapture>,
}

impl CaptureProxy {
    /// Binds the proxy listener immediately (so its public URL is known before
    /// the server harness starts); call `set_target` once READY arrives.
    pub async fn bind() -> anyhow::Result<(Arc<Self>, u16)> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        let proxy = Arc::new(CaptureProxy {
            state: Arc::new(Mutex::new(ProxyState {
                target_port: None,
                current: String::new(),
                captures: HashMap::new(),
            })),
        });
        let accept_proxy = proxy.clone();
        tokio::spawn(async move {
            loop {
                let Ok((inbound, _)) = listener.accept().await else {
                    break;
                };
                let proxy = accept_proxy.clone();
                tokio::spawn(async move {
                    let _ = proxy.handle(inbound).await;
                });
            }
        });
        Ok((proxy, port))
    }

    pub fn set_target(&self, port: u16) {
        self.state.lock().unwrap().target_port = Some(port);
    }

    pub fn select_scenario(&self, id: &str) {
        let mut state = self.state.lock().unwrap();
        state.current = id.to_string();
        state.captures.entry(id.to_string()).or_default();
    }

    pub fn capture_for(&self, id: &str) -> Option<WireCapture> {
        self.state.lock().unwrap().captures.get(id).cloned()
    }

    async fn handle(&self, mut inbound: TcpStream) -> anyhow::Result<()> {
        let target = {
            let state = self.state.lock().unwrap();
            state.target_port
        };
        let Some(port) = target else {
            return Ok(());
        };
        let mut outbound = TcpStream::connect(("127.0.0.1", port)).await?;
        let (mut in_read, mut in_write) = inbound.split();
        let (mut out_read, mut out_write) = outbound.split();

        let state_up = self.state.clone();
        let state_down = self.state.clone();

        let upstream = async {
            let mut buf = [0u8; 16 * 1024];
            loop {
                let n = in_read.read(&mut buf).await?;
                if n == 0 {
                    break;
                }
                {
                    let mut s = state_up.lock().unwrap();
                    let key = s.current.clone();
                    s.captures
                        .entry(key)
                        .or_default()
                        .client_to_server
                        .extend_from_slice(&buf[..n]);
                }
                out_write.write_all(&buf[..n]).await?;
            }
            out_write.shutdown().await?;
            Ok::<_, anyhow::Error>(())
        };
        let downstream = async {
            let mut buf = [0u8; 16 * 1024];
            loop {
                let n = out_read.read(&mut buf).await?;
                if n == 0 {
                    break;
                }
                {
                    let mut s = state_down.lock().unwrap();
                    let key = s.current.clone();
                    s.captures
                        .entry(key)
                        .or_default()
                        .server_to_client
                        .extend_from_slice(&buf[..n]);
                }
                in_write.write_all(&buf[..n]).await?;
            }
            in_write.shutdown().await?;
            Ok::<_, anyhow::Error>(())
        };
        let _ = tokio::join!(upstream, downstream);
        Ok(())
    }
}
